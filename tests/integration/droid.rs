use crate::test_utils::fixture_path;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use git_ai::transcripts::agent::Agent;
use git_ai::transcripts::agents::DroidAgent;
use git_ai::transcripts::watermark::HybridWatermark;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;

fn parse_droid(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("droid")?.parse(hook_input, "t_test")
}

#[test]
fn test_parse_droid_jsonl_transcript() {
    let fixture = fixture_path("droid-session.jsonl");
    let agent = DroidAgent;
    let watermark = Box::new(HybridWatermark::new(0, 0, None));
    let result = agent
        .read_incremental(fixture.as_path(), watermark, "test")
        .expect("Failed to parse JSONL");

    // Verify we parsed some events (one per JSONL line)
    assert!(!result.events.is_empty(), "Result should contain events");

    // Verify the fixture has the expected number of raw events (only "message" type lines)
    assert_eq!(result.events.len(), 10, "Should have 10 raw message events");

    // Verify correct event types exist in the raw JSON
    let has_user = result.events.iter().any(|e| {
        e["type"].as_str() == Some("message") && e["message"]["role"].as_str() == Some("user")
    });
    let has_assistant = result.events.iter().any(|e| {
        e["type"].as_str() == Some("message") && e["message"]["role"].as_str() == Some("assistant")
    });
    let has_tool_use = result.events.iter().any(|e| {
        e["type"].as_str() == Some("message")
            && e["message"]["content"]
                .as_array()
                .map(|arr| arr.iter().any(|c| c["type"].as_str() == Some("tool_use")))
                .unwrap_or(false)
    });

    assert!(has_user, "Should have user message events");
    assert!(has_assistant, "Should have assistant message events");
    assert!(has_tool_use, "Should have tool_use events");

    // Verify timestamps are present on message events
    for event in &result.events {
        if event["type"].as_str() == Some("message") {
            assert!(
                event["timestamp"].as_str().is_some(),
                "Message events should have a timestamp"
            );
        }
    }
}

#[test]
fn test_parse_droid_settings_model() {
    let fixture = fixture_path("droid-session.settings.json");
    let content = fs::read_to_string(fixture).expect("Failed to read settings.json");
    let settings: serde_json::Value =
        serde_json::from_str(&content).expect("Failed to parse settings.json");
    let model = settings["model"].as_str().map(|s| s.to_string());

    assert!(model.is_some(), "Model should be extracted from settings");
    assert_eq!(
        model.unwrap(),
        "custom:BYOK-GPT-5-MINI-0",
        "Model should match the fixture value"
    );
}

#[test]
fn test_droid_preset_extracts_edited_filepath() {
    let fixture = fixture_path("droid-session.jsonl");
    let settings_fixture = fixture_path("droid-session.settings.json");

    let transcript_path = fixture.to_str().unwrap();
    let settings_path = settings_fixture.to_str().unwrap();

    let temp_dir = tempfile::tempdir().unwrap();
    let jsonl_path = temp_dir.path().join("session.jsonl");
    let temp_settings_path = temp_dir.path().join("session.settings.json");
    fs::copy(transcript_path, &jsonl_path).unwrap();
    fs::copy(settings_path, &temp_settings_path).unwrap();

    let hook_input = json!({
        "cwd": "/Users/testuser/projects/testing-git",
        "hookEventName": "PostToolUse",
        "sessionId": "052cb8d0-4616-488a-99fe-bfbbbe9429b3",
        "toolName": "ApplyPatch",
        "tool_input": {
            "file_path": "/Users/testuser/projects/testing-git/index.ts"
        },
        "transcriptPath": jsonl_path.to_str().unwrap()
    })
    .to_string();

    let events = parse_droid(&hook_input).expect("Failed to parse droid hook input");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(!e.file_paths.is_empty());
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("index.ts")),
                "Should contain edited filepath, got: {:?}",
                e.file_paths
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_droid_preset_extracts_applypatch_filepath() {
    let temp_dir = tempfile::tempdir().unwrap();
    let jsonl_path = temp_dir.path().join("session.jsonl");
    let settings_path = temp_dir.path().join("session.settings.json");

    fs::write(&jsonl_path, "").unwrap();
    fs::write(&settings_path, r#"{"model":"test-model"}"#).unwrap();

    let hook_input = json!({
        "cwd": "/Users/testuser/projects/testing-git",
        "hookEventName": "PostToolUse",
        "sessionId": "test-session-id",
        "toolName": "ApplyPatch",
        "tool_input": "*** Begin Patch\n*** Update File: /Users/testuser/projects/testing-git/index.ts\n@@\n-// old\n+// new\n*** End Patch",
        "transcriptPath": jsonl_path.to_str().unwrap()
    })
    .to_string();

    let events = parse_droid(&hook_input).expect("Failed to parse droid hook input");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            let path_strs: Vec<String> = e
                .file_paths
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            assert!(
                path_strs.iter().any(|p| p.contains("index.ts")),
                "Should extract file path from ApplyPatch text, got: {:?}",
                path_strs
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_droid_preset_stores_metadata_paths() {
    let temp_dir = tempfile::tempdir().unwrap();
    let jsonl_path = temp_dir.path().join("session.jsonl");
    let settings_path = temp_dir.path().join("session.settings.json");

    let fixture = fixture_path("droid-session.jsonl");
    let settings_fixture = fixture_path("droid-session.settings.json");
    fs::copy(&fixture, &jsonl_path).unwrap();
    fs::copy(&settings_fixture, &settings_path).unwrap();

    let hook_input = json!({
        "cwd": "/Users/testuser/projects/testing-git",
        "hookEventName": "PostToolUse",
        "sessionId": "052cb8d0-4616-488a-99fe-bfbbbe9429b3",
        "toolName": "Read",
        "transcriptPath": jsonl_path.to_str().unwrap()
    })
    .to_string();

    let events = parse_droid(&hook_input).expect("Failed to parse droid hook input");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(
                e.context.metadata.contains_key("transcript_path"),
                "Metadata should contain transcript_path"
            );
            assert!(
                e.context.metadata.contains_key("settings_path"),
                "Metadata should contain settings_path"
            );
            assert_eq!(
                e.context.metadata["transcript_path"],
                jsonl_path.to_str().unwrap()
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_droid_preset_uses_raw_session_id() {
    let temp_dir = tempfile::tempdir().unwrap();
    let jsonl_path = temp_dir.path().join("session.jsonl");
    let settings_path = temp_dir.path().join("session.settings.json");

    fs::write(&jsonl_path, "").unwrap();
    fs::write(&settings_path, r#"{"model":"test-model"}"#).unwrap();

    let session_uuid = "052cb8d0-4616-488a-99fe-bfbbbe9429b3";

    let hook_input = json!({
        "cwd": "/Users/testuser/projects/testing-git",
        "hookEventName": "PostToolUse",
        "sessionId": session_uuid,
        "toolName": "Read",
        "transcriptPath": jsonl_path.to_str().unwrap()
    })
    .to_string();

    let events = parse_droid(&hook_input).expect("Failed to parse droid hook input");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(
                e.context.agent_id.id, session_uuid,
                "agent_id.id should be the raw session UUID"
            );
            assert_eq!(e.context.agent_id.tool, "droid");
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_droid_jsonl_skips_non_message_entries() {
    // Droid agent filters to only "message" type entries, skipping session_start, todo_state, etc.
    let jsonl_content = r#"{"type":"session_start","id":"abc","title":"Test","cwd":"/tmp"}
{"type":"message","id":"msg1","timestamp":"2026-01-28T16:57:01.391Z","message":{"role":"user","content":[{"type":"text","text":"Hello"}]}}
{"type":"todo_state","id":"todo1","timestamp":"2026-01-28T16:57:02.000Z","todos":{"todos":"1. test"}}
{"type":"message","id":"msg2","timestamp":"2026-01-28T16:57:03.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Hi there!"}]}}
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();

    let agent = DroidAgent;
    let watermark = Box::new(HybridWatermark::new(0, 0, None));
    let result = agent
        .read_incremental(temp_file.path(), watermark, "test")
        .expect("Failed to parse JSONL");

    // Only "message" type entries are returned (session_start and todo_state are skipped)
    assert_eq!(
        result.events.len(),
        2,
        "Should return only 2 message events, got {} events",
        result.events.len()
    );

    // Verify the message types
    assert_eq!(result.events[0]["type"].as_str(), Some("message"));
    assert_eq!(result.events[0]["message"]["role"].as_str(), Some("user"));
    assert_eq!(result.events[1]["type"].as_str(), Some("message"));
    assert_eq!(
        result.events[1]["message"]["role"].as_str(),
        Some("assistant"),
    );
}

#[test]
fn test_droid_tool_results_are_not_parsed_as_user_messages() {
    // With raw JSON, tool_result lines ARE included as raw events.
    let jsonl_content = r#"{"type":"message","id":"msg1","timestamp":"2026-01-28T16:57:16.179Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_123","content":"File read successfully"}]}}
{"type":"message","id":"msg2","timestamp":"2026-01-28T16:57:17.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Done!"}]}}
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();

    let agent = DroidAgent;
    let watermark = Box::new(HybridWatermark::new(0, 0, None));
    let result = agent
        .read_incremental(temp_file.path(), watermark, "test")
        .expect("Failed to parse JSONL");

    // Both lines are returned as raw events
    assert_eq!(
        result.events.len(),
        2,
        "Both JSONL lines should be returned as raw events"
    );

    assert_eq!(result.events[0]["type"].as_str(), Some("message"),);
    assert_eq!(result.events[0]["message"]["role"].as_str(), Some("user"),);
    assert_eq!(result.events[1]["type"].as_str(), Some("message"),);
    assert_eq!(
        result.events[1]["message"]["role"].as_str(),
        Some("assistant"),
    );
    assert_eq!(
        result.events[1]["message"]["content"][0]["text"].as_str(),
        Some("Done!"),
        "Assistant response text should be 'Done!'"
    );
}

#[test]
fn test_droid_e2e_prefers_latest_checkpoint_for_prompts() {
    use crate::repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });

    let repo_root = repo.canonical_path();

    let src_dir = repo_root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("main.ts");
    fs::write(&file_path, "// initial\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let transcript_path = repo_root.join("droid-session.jsonl");
    let settings_path = repo_root.join("droid-session.settings.json");

    fs::write(&transcript_path, "").unwrap();
    fs::write(&settings_path, r#"{"model":"custom:BYOK-GPT-5-MINI-0"}"#).unwrap();

    let hook_input = json!({
        "cwd": repo_root.to_string_lossy().to_string(),
        "hookEventName": "PostToolUse",
        "sessionId": "052cb8d0-4616-488a-99fe-bfbbbe9429b3",
        "toolName": "ApplyPatch",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcriptPath": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    fs::write(&file_path, "// initial\n// ai line one\n").unwrap();
    repo.git_ai(&["checkpoint", "droid", "--hook-input", &hook_input])
        .unwrap();

    let fixture = fixture_path("droid-session.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();
    fs::write(&file_path, "// initial\n// ai line one\n// ai line two\n").unwrap();
    repo.git_ai(&["checkpoint", "droid", "--hook-input", &hook_input])
        .unwrap();

    let commit = repo.stage_all_and_commit("Add AI lines").unwrap();

    assert_eq!(
        commit.authorship_log.metadata.sessions.len(),
        1,
        "Expected a single session record"
    );
    let session_record = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("Session record should exist");

    assert_eq!(
        session_record.agent_id.model, "unknown",
        "Session record model comes from preset AgentId (model resolution not wired up)"
    );
}

#[test]
fn test_droid_preset_pretooluse_returns_human_checkpoint() {
    let temp_dir = tempfile::tempdir().unwrap();
    let jsonl_path = temp_dir.path().join("session.jsonl");
    let settings_path = temp_dir.path().join("session.settings.json");

    fs::write(&jsonl_path, "").unwrap();
    fs::write(&settings_path, r#"{"model":"test-model"}"#).unwrap();

    let hook_input = json!({
        "cwd": "/Users/testuser/projects/testing-git",
        "hookEventName": "PreToolUse",
        "sessionId": "052cb8d0-4616-488a-99fe-bfbbbe9429b3",
        "toolName": "ApplyPatch",
        "tool_input": {
            "file_path": "/Users/testuser/projects/testing-git/index.ts"
        },
        "transcriptPath": jsonl_path.to_str().unwrap()
    })
    .to_string();

    let events = parse_droid(&hook_input).expect("Failed to parse droid hook input");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(
                e.context.cwd,
                PathBuf::from("/Users/testuser/projects/testing-git")
            );
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("index.ts")),
                "will_edit_filepaths should contain the target file"
            );
        }
        _ => panic!("Expected PreFileEdit for PreToolUse"),
    }
}

#[test]
fn test_droid_settings_missing_model_field() {
    let mut temp = NamedTempFile::new().unwrap();
    temp.write_all(b"{}").unwrap();
    let content = fs::read_to_string(temp.path()).expect("Should read settings file");
    let settings: serde_json::Value = serde_json::from_str(&content).expect("Should parse JSON");
    let model = settings["model"].as_str().map(|s| s.to_string());
    assert!(model.is_none(), "Missing model field should return None");
}

#[test]
fn test_droid_jsonl_parses_thinking_blocks() {
    // With raw JSON, a single JSONL line with thinking+text content blocks = 1 raw event.
    let jsonl = r#"{"type":"message","id":"m1","timestamp":"2026-01-28T17:00:00.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Let me think about this..."},{"type":"text","text":"Here is my answer."}]}}
"#;
    let mut temp = NamedTempFile::new().unwrap();
    temp.write_all(jsonl.as_bytes()).unwrap();

    let agent = DroidAgent;
    let watermark = Box::new(HybridWatermark::new(0, 0, None));
    let result = agent
        .read_incremental(temp.path(), watermark, "test")
        .expect("Failed to parse JSONL");

    // Single JSONL line = single raw event
    assert_eq!(
        result.events.len(),
        1,
        "Single JSONL line should produce single raw event"
    );

    // Verify the content blocks are preserved in the raw event
    let content = result.events[0]["message"]["content"]
        .as_array()
        .expect("content should be an array");
    assert_eq!(content.len(), 2, "Should have 2 content blocks");

    assert_eq!(
        content[0]["type"].as_str(),
        Some("thinking"),
        "First content block should be thinking"
    );
    let thinking_text = content[0]["thinking"]
        .as_str()
        .expect("thinking block should have text");
    assert!(
        thinking_text.contains("think"),
        "Thinking block should contain 'think', got: {}",
        thinking_text
    );

    assert_eq!(
        content[1]["type"].as_str(),
        Some("text"),
        "Second content block should be text"
    );
    assert_eq!(
        content[1]["text"].as_str(),
        Some("Here is my answer."),
        "Text block should contain the answer"
    );
}

crate::reuse_tests_in_worktree!(
    test_parse_droid_jsonl_transcript,
    test_parse_droid_settings_model,
    test_droid_preset_extracts_edited_filepath,
    test_droid_preset_extracts_applypatch_filepath,
    test_droid_preset_stores_metadata_paths,
    test_droid_preset_uses_raw_session_id,
    test_droid_jsonl_skips_non_message_entries,
    test_droid_tool_results_are_not_parsed_as_user_messages,
    test_droid_e2e_prefers_latest_checkpoint_for_prompts,
    test_droid_preset_pretooluse_returns_human_checkpoint,
    test_droid_settings_missing_model_field,
    test_droid_jsonl_parses_thinking_blocks,
);
