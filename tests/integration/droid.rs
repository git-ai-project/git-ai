use crate::test_utils::fixture_path;
use git_ai::authorship::transcript::Message;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::commands::checkpoint_agent::transcript_readers;
use git_ai::error::GitAiError;
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
    let (transcript, model) =
        transcript_readers::read_droid_jsonl(fixture.as_path()).expect("Failed to parse JSONL");

    // Verify we parsed some messages
    assert!(
        !transcript.messages().is_empty(),
        "Transcript should contain messages"
    );

    // Model should be None — Droid stores model in .settings.json, not JSONL
    assert!(
        model.is_none(),
        "Model should be None (comes from settings.json, not JSONL)"
    );

    // Verify correct message types exist
    let has_user = transcript
        .messages()
        .iter()
        .any(|m| matches!(m, Message::User { .. }));
    let has_assistant = transcript
        .messages()
        .iter()
        .any(|m| matches!(m, Message::Assistant { .. }));
    let has_tool_use = transcript
        .messages()
        .iter()
        .any(|m| matches!(m, Message::ToolUse { .. }));

    assert!(has_user, "Should have user messages");
    assert!(has_assistant, "Should have assistant messages");
    assert!(has_tool_use, "Should have tool_use messages");

    // Verify timestamps are ISO 8601 strings
    for message in transcript.messages() {
        match message {
            Message::User { timestamp, .. }
            | Message::Assistant { timestamp, .. }
            | Message::ToolUse { timestamp, .. }
            | Message::Thinking { timestamp, .. }
            | Message::Plan { timestamp, .. } => {
                if let Some(ts) = timestamp {
                    assert!(
                        ts.contains("T") && ts.contains("Z"),
                        "Timestamp should be ISO 8601 format, got: {}",
                        ts
                    );
                }
            }
        }
    }
}

#[test]
fn test_parse_droid_settings_model() {
    let fixture = fixture_path("droid-session.settings.json");
    let model = transcript_readers::read_droid_model_from_settings(fixture.as_path())
        .expect("Failed to parse settings.json");

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
    let jsonl_content = r#"{"type":"session_start","id":"abc","title":"Test","cwd":"/tmp"}
{"type":"message","id":"msg1","timestamp":"2026-01-28T16:57:01.391Z","message":{"role":"user","content":[{"type":"text","text":"Hello"}]}}
{"type":"todo_state","id":"todo1","timestamp":"2026-01-28T16:57:02.000Z","todos":{"todos":"1. test"}}
{"type":"message","id":"msg2","timestamp":"2026-01-28T16:57:03.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Hi there!"}]}}
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();

    let (transcript, _model) =
        transcript_readers::read_droid_jsonl(temp_file.path()).expect("Failed to parse JSONL");

    assert_eq!(
        transcript.messages().len(),
        2,
        "Should only parse 'message' type entries, got {} messages",
        transcript.messages().len()
    );

    assert!(
        matches!(transcript.messages()[0], Message::User { .. }),
        "First message should be User"
    );
    assert!(
        matches!(transcript.messages()[1], Message::Assistant { .. }),
        "Second message should be Assistant"
    );
}

#[test]
fn test_droid_tool_results_are_not_parsed_as_user_messages() {
    let jsonl_content = r#"{"type":"message","id":"msg1","timestamp":"2026-01-28T16:57:16.179Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_123","content":"File read successfully"}]}}
{"type":"message","id":"msg2","timestamp":"2026-01-28T16:57:17.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Done!"}]}}
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();

    let (transcript, _model) =
        transcript_readers::read_droid_jsonl(temp_file.path()).expect("Failed to parse JSONL");

    assert_eq!(
        transcript.messages().len(),
        1,
        "Tool results should not be parsed as user messages"
    );

    assert!(
        matches!(transcript.messages()[0], Message::Assistant { .. }),
        "Only message should be Assistant"
    );
    if let Message::Assistant { text, .. } = &transcript.messages()[0] {
        assert_eq!(text, "Done!");
    }
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

    assert!(
        !session_record.messages.is_empty(),
        "Session record should contain messages from the latest checkpoint"
    );
    assert_eq!(
        session_record.agent_id.model, "custom:BYOK-GPT-5-MINI-0",
        "Session record should use the model from settings.json"
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
    let result = transcript_readers::read_droid_model_from_settings(temp.path())
        .expect("Should not error on missing model");
    assert!(result.is_none(), "Missing model field should return None");
}

#[test]
fn test_droid_jsonl_parses_thinking_blocks() {
    let jsonl = r#"{"type":"message","id":"m1","timestamp":"2026-01-28T17:00:00.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Let me think about this..."},{"type":"text","text":"Here is my answer."}]}}
"#;
    let mut temp = NamedTempFile::new().unwrap();
    temp.write_all(jsonl.as_bytes()).unwrap();
    let (transcript, _) =
        transcript_readers::read_droid_jsonl(temp.path()).expect("Failed to parse JSONL");
    assert_eq!(
        transcript.messages().len(),
        2,
        "Should parse both thinking and text blocks"
    );
    if let Message::Assistant { text, .. } = &transcript.messages()[0] {
        assert!(
            text.contains("think"),
            "First message should be the thinking block"
        );
    } else {
        panic!("Expected Assistant (thinking)");
    }
    assert!(
        matches!(transcript.messages()[1], Message::Assistant { .. }),
        "Second message should be Assistant text"
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
