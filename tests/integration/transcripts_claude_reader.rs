//! Integration tests for Claude Code transcript reader.

use git_ai::transcripts::agent::Agent;
use git_ai::transcripts::agents::ClaudeAgent;
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
fn test_claude_reader_with_fixture() {
    let path = fixture_path("claude_simple.jsonl");
    let watermark = Box::new(ByteOffsetWatermark::new(0));

    let agent = ClaudeAgent;
    let result = agent
        .read_incremental(&path, watermark, "test-session")
        .unwrap();

    // Should have 3 raw events (one per JSONL line)
    assert_eq!(result.events.len(), 3);

    // Event 0: User message
    let event0 = &result.events[0];
    assert_eq!(event0["type"].as_str(), Some("user"));
    assert_eq!(
        event0["message"]["content"].as_str(),
        Some("Write a hello world function")
    );
    assert!(event0["timestamp"].as_str().is_some());

    // Event 1: Assistant text
    let event1 = &result.events[1];
    assert_eq!(event1["type"].as_str(), Some("assistant"));
    assert_eq!(
        event1["message"]["content"][0]["text"].as_str(),
        Some("I'll create a hello world function for you.")
    );

    // Event 2: Assistant with tool use
    let event2 = &result.events[2];
    assert_eq!(event2["type"].as_str(), Some("assistant"));
    assert_eq!(
        event2["message"]["content"][0]["name"].as_str(),
        Some("Write")
    );
    assert_eq!(
        event2["message"]["content"][0]["id"].as_str(),
        Some("toolu_abc123")
    );
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
    let agent = ClaudeAgent;
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

    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent.read_incremental(&file_path, watermark, "test-session");

    assert!(result.is_err());
    if let Err(e) = result {
        match e {
            git_ai::transcripts::types::TranscriptError::Parse { line, message } => {
                assert_eq!(line, 1);
                assert!(message.contains("Invalid JSON"));
            }
            _ => panic!("Expected Parse error, got {:?}", e),
        }
    }
}

#[test]
fn test_claude_reader_file_not_found() {
    let path = PathBuf::from("/nonexistent/transcript.jsonl");
    let agent = ClaudeAgent;
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
fn test_claude_reader_thinking_blocks() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("thinking.jsonl");

    let mut file = File::create(&file_path).unwrap();
    writeln!(
        file,
        r#"{{"type":"assistant","message":{{"content":[{{"type":"thinking","thinking":"Let me think about this..."}}],"model":"claude-sonnet-4"}},"timestamp":"2025-01-01T00:00:00Z"}}"#
    )
    .unwrap();
    file.flush().unwrap();

    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(&file_path, watermark, "test-session")
        .unwrap();

    assert_eq!(result.events.len(), 1);
    assert_eq!(result.events[0]["type"].as_str(), Some("assistant"));
    assert_eq!(
        result.events[0]["message"]["content"][0]["type"].as_str(),
        Some("thinking")
    );
    assert_eq!(
        result.events[0]["message"]["content"][0]["thinking"].as_str(),
        Some("Let me think about this...")
    );
}

#[test]
fn test_claude_reader_mixed_content() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("mixed.jsonl");

    let mut file = File::create(&file_path).unwrap();
    writeln!(
        file,
        r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Here's the code:"}},{{"type":"tool_use","name":"Write","id":"toolu_xyz"}},{{"type":"text","text":"Done!"}}],"model":"claude-sonnet-4"}},"timestamp":"2025-01-01T00:00:00Z"}}"#
    )
    .unwrap();
    file.flush().unwrap();

    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(&file_path, watermark, "test-session")
        .unwrap();

    // Should have 1 raw event (one JSONL line with mixed content blocks)
    assert_eq!(result.events.len(), 1);
    let event = &result.events[0];
    assert_eq!(event["type"].as_str(), Some("assistant"));

    let content = event["message"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 3);
    assert_eq!(content[0]["type"].as_str(), Some("text"));
    assert_eq!(content[0]["text"].as_str(), Some("Here's the code:"));
    assert_eq!(content[1]["type"].as_str(), Some("tool_use"));
    assert_eq!(content[1]["name"].as_str(), Some("Write"));
    assert_eq!(content[1]["id"].as_str(), Some("toolu_xyz"));
    assert_eq!(content[2]["type"].as_str(), Some("text"));
    assert_eq!(content[2]["text"].as_str(), Some("Done!"));
}
