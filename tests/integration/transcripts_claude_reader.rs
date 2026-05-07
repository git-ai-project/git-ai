//! Integration tests for Claude Code transcript reader.

use git_ai::transcripts::agent::Agent;
use git_ai::transcripts::agents::ClaudeAgent;
use git_ai::transcripts::agents::claude::ClaudeAgent as ClaudeAgentImpl;
use git_ai::transcripts::watermark::ByteOffsetWatermark;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use tempfile::TempDir;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("transcripts")
        .join("fixtures")
        .join(name)
}

#[test]
fn test_claude_reader_raw_event_fidelity() {
    let path = fixture_path("claude_simple.jsonl");
    let agent = ClaudeAgent::new();
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(&path, watermark, "test-session")
        .unwrap();

    let expected: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert_eq!(result.events, expected);
}

#[test]
fn test_claude_reader_watermark_resume() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("transcript.jsonl");

    // Write initial content
    let mut file = File::create(&file_path).unwrap();
    writeln!(
        file,
        r#"{{"type":"user","message":{{"content":"First message"}},"timestamp":"2025-01-01T00:00:00Z"}}"#
    )
    .unwrap();
    file.flush().unwrap();
    drop(file);

    // Read from start
    let agent = ClaudeAgent::new();
    let watermark1 = Box::new(ByteOffsetWatermark::new(0));
    let result1 = agent
        .read_incremental(&file_path, watermark1, "test-session")
        .unwrap();
    assert_eq!(result1.events.len(), 1);

    // Save watermark position
    let offset_after_first = result1.new_watermark.serialize().parse::<u64>().unwrap();

    // Append more content
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&file_path)
        .unwrap();
    writeln!(
        file,
        r#"{{"type":"user","message":{{"content":"Second message"}},"timestamp":"2025-01-01T00:00:01Z"}}"#
    )
    .unwrap();
    file.flush().unwrap();
    drop(file);

    // Read from watermark - should only get new line
    let watermark2 = Box::new(ByteOffsetWatermark::new(offset_after_first));
    let result2 = agent
        .read_incremental(&file_path, watermark2, "test-session")
        .unwrap();
    assert_eq!(result2.events.len(), 1);
    assert_eq!(
        result2.events[0]["message"]["content"].as_str(),
        Some("Second message")
    );

    // Verify watermark advanced
    let offset_after_second = result2.new_watermark.serialize().parse::<u64>().unwrap();
    assert!(offset_after_second > offset_after_first);
}

#[test]
fn test_claude_reader_handles_malformed_json() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("malformed.jsonl");

    let mut file = File::create(&file_path).unwrap();
    writeln!(file, "{{invalid json syntax}}").unwrap();
    file.flush().unwrap();

    let agent = ClaudeAgent::new();
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent.read_incremental(&file_path, watermark, "test-session");

    // Malformed JSON lines are skipped, not fatal errors
    let batch = result.expect("malformed lines should be skipped, not cause errors");
    assert_eq!(batch.events.len(), 0);
}

#[test]
fn test_claude_reader_file_not_found() {
    let path = PathBuf::from("/nonexistent/transcript.jsonl");
    let agent = ClaudeAgent::new();
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent.read_incremental(&path, watermark, "test-session");

    assert!(result.is_err());
    if let Err(e) = result {
        match e {
            git_ai::transcripts::types::TranscriptError::Fatal { message } => {
                assert!(message.contains("not found"));
            }
            _ => panic!("Expected Fatal error, got {:?}", e),
        }
    }
}

#[test]
fn test_claude_transcript_ids_extracted_from_fixture() {
    let fixture = fixture_path("claude_with_ids.jsonl");
    let agent = ClaudeAgent::new();
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let batch = agent
        .read_incremental(&fixture, watermark, "sess-parent-abc")
        .unwrap();

    assert_eq!(batch.events.len(), 5);

    // Event 0: user message — has uuid, no tool_use
    let (eid, pid, tid) = agent.extract_event_ids(&batch.events[0]);
    assert_eq!(
        eid,
        Some("aaa11111-1111-1111-1111-111111111111".to_string())
    );
    assert_eq!(pid, None);
    assert_eq!(tid, None);

    // Event 1: assistant text — has uuid + parentUuid, no tool_use
    let (eid, pid, tid) = agent.extract_event_ids(&batch.events[1]);
    assert_eq!(
        eid,
        Some("bbb22222-2222-2222-2222-222222222222".to_string())
    );
    assert_eq!(
        pid,
        Some("aaa11111-1111-1111-1111-111111111111".to_string())
    );
    assert_eq!(tid, None);

    // Event 2: assistant with tool_use — has uuid + parentUuid + tool_use id
    let (eid, pid, tid) = agent.extract_event_ids(&batch.events[2]);
    assert_eq!(
        eid,
        Some("ccc33333-3333-3333-3333-333333333333".to_string())
    );
    assert_eq!(
        pid,
        Some("bbb22222-2222-2222-2222-222222222222".to_string())
    );
    assert_eq!(tid, Some("toolu_01AbCdEfGhIjKlMnOp".to_string()));

    // Event 3: user with tool_result — has uuid + parentUuid + tool_use_id
    let (eid, pid, tid) = agent.extract_event_ids(&batch.events[3]);
    assert_eq!(
        eid,
        Some("ddd44444-4444-4444-4444-444444444444".to_string())
    );
    assert_eq!(
        pid,
        Some("ccc33333-3333-3333-3333-333333333333".to_string())
    );
    assert_eq!(tid, Some("toolu_01AbCdEfGhIjKlMnOp".to_string()));

    // Event 4: assistant text (final) — has uuid + parentUuid, no tool_use
    let (eid, pid, tid) = agent.extract_event_ids(&batch.events[4]);
    assert_eq!(
        eid,
        Some("eee55555-5555-5555-5555-555555555555".to_string())
    );
    assert_eq!(
        pid,
        Some("ddd44444-4444-4444-4444-444444444444".to_string())
    );
    assert_eq!(tid, None);
}

#[test]
fn test_claude_subagent_parent_detection_from_path() {
    let subagent_path = PathBuf::from(
        "/home/user/.claude/projects/proj/sess-parent-abc/subagents/agent-a1b2c3d4e5f6.jsonl",
    );
    assert_eq!(
        ClaudeAgentImpl::detect_subagent_parent(&subagent_path),
        Some("sess-parent-abc".to_string())
    );

    let normal_path = PathBuf::from("/home/user/.claude/projects/proj/sess-parent-abc.jsonl");
    assert_eq!(ClaudeAgentImpl::detect_subagent_parent(&normal_path), None);

    let edge_path = PathBuf::from("subagents/agent-foo.jsonl");
    assert_eq!(ClaudeAgentImpl::detect_subagent_parent(&edge_path), None);
}

#[test]
fn test_claude_subagent_transcript_reads_with_correct_ids() {
    let subagent_fixture =
        fixture_path("claude_subagent/sess-parent-abc/subagents/agent-a1b2c3d4e5f6.jsonl");
    let agent = ClaudeAgent::new();
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let batch = agent
        .read_incremental(&subagent_fixture, watermark, "agent-a1b2c3d4e5f6")
        .unwrap();

    assert_eq!(batch.events.len(), 3);

    // Subagent event has tool_use in content
    let (eid, pid, tid) = agent.extract_event_ids(&batch.events[1]);
    assert_eq!(
        eid,
        Some("sub22222-2222-2222-2222-222222222222".to_string())
    );
    assert_eq!(
        pid,
        Some("sub11111-1111-1111-1111-111111111111".to_string())
    );
    assert_eq!(tid, Some("toolu_sub_search_01".to_string()));

    // tool_result event
    let (eid, pid, tid) = agent.extract_event_ids(&batch.events[2]);
    assert_eq!(
        eid,
        Some("sub33333-3333-3333-3333-333333333333".to_string())
    );
    assert_eq!(
        pid,
        Some("sub22222-2222-2222-2222-222222222222".to_string())
    );
    assert_eq!(tid, Some("toolu_sub_search_01".to_string()));
}
