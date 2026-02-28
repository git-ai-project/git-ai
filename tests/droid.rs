#[macro_use]
mod repos;
mod test_utils;

use git_ai::authorship::transcript::Message;
use git_ai::authorship::working_log::CheckpointKind;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, DroidPreset,
};
use serde_json::json;
use std::fs;
use std::io::Write;
use tempfile::NamedTempFile;
use test_utils::fixture_path;

#[test]
fn test_parse_droid_jsonl_transcript() {
    let fixture = fixture_path("droid-session.jsonl");
    let (transcript, model) =
        DroidPreset::transcript_and_model_from_droid_jsonl(fixture.to_str().unwrap())
            .expect("Failed to parse JSONL");

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
    let model = DroidPreset::model_from_droid_settings_json(fixture.to_str().unwrap())
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
    // Create a temporary JSONL file for the transcript
    let fixture = fixture_path("droid-session.jsonl");
    let settings_fixture = fixture_path("droid-session.settings.json");

    // Build settings path as sibling of transcript
    let transcript_path = fixture.to_str().unwrap();
    let settings_path = settings_fixture.to_str().unwrap();

    // We need the transcript_path to end with .jsonl and settings as sibling
    // Create temp files to control the naming
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

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input),
    };

    let preset = DroidPreset;
    let result = preset.run(flags).expect("Failed to run DroidPreset");

    assert!(result.edited_filepaths.is_some());
    let edited = result.edited_filepaths.unwrap();
    assert_eq!(edited.len(), 1);
    assert_eq!(edited[0], "/Users/testuser/projects/testing-git/index.ts");
}

#[test]
fn test_droid_preset_extracts_applypatch_filepath() {
    let temp_dir = tempfile::tempdir().unwrap();
    let jsonl_path = temp_dir.path().join("session.jsonl");
    let settings_path = temp_dir.path().join("session.settings.json");

    // Create minimal valid JSONL and settings
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

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input),
    };

    let preset = DroidPreset;
    let result = preset.run(flags).expect("Failed to run DroidPreset");

    assert!(result.edited_filepaths.is_some());
    let edited = result.edited_filepaths.unwrap();
    assert!(
        edited.contains(&"/Users/testuser/projects/testing-git/index.ts".to_string()),
        "Should extract file path from ApplyPatch text, got: {:?}",
        edited
    );
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

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input),
    };

    let preset = DroidPreset;
    let result = preset.run(flags).expect("Failed to run DroidPreset");

    assert!(result.agent_metadata.is_some());
    let metadata = result.agent_metadata.unwrap();
    assert!(
        metadata.contains_key("transcript_path"),
        "Metadata should contain transcript_path"
    );
    assert!(
        metadata.contains_key("settings_path"),
        "Metadata should contain settings_path"
    );
    assert_eq!(metadata["transcript_path"], jsonl_path.to_str().unwrap());
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

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input),
    };

    let preset = DroidPreset;
    let result = preset.run(flags).expect("Failed to run DroidPreset");

    assert_eq!(
        result.agent_id.id, session_uuid,
        "agent_id.id should be the raw session UUID"
    );
    assert_eq!(result.agent_id.tool, "droid");
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
    let temp_path = temp_file.path().to_str().unwrap();

    let (transcript, _model) = DroidPreset::transcript_and_model_from_droid_jsonl(temp_path)
        .expect("Failed to parse JSONL");

    // Should only have 2 messages (from the two "message" type entries)
    // session_start and todo_state should be skipped
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
    // User message containing only tool_result items should produce no user messages
    let jsonl_content = r#"{"type":"message","id":"msg1","timestamp":"2026-01-28T16:57:16.179Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_123","content":"File read successfully"}]}}
{"type":"message","id":"msg2","timestamp":"2026-01-28T16:57:17.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Done!"}]}}
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let (transcript, _model) = DroidPreset::transcript_and_model_from_droid_jsonl(temp_path)
        .expect("Failed to parse JSONL");

    // Should only have 1 message (the assistant response)
    // The tool_result should be skipped entirely
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
    use repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });

    let repo_root = repo.canonical_path();

    // Create initial file and commit
    let src_dir = repo_root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("main.ts");
    fs::write(&file_path, "// initial\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Set up transcript and settings files in the repo dir
    let transcript_path = repo_root.join("droid-session.jsonl");
    let settings_path = repo_root.join("droid-session.settings.json");

    // First checkpoint: empty transcript (simulates race where data isn't ready)
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

    // First AI edit with empty transcript
    fs::write(&file_path, "// initial\n// ai line one\n").unwrap();
    repo.git_ai(&["checkpoint", "droid", "--hook-input", &hook_input])
        .unwrap();

    // Second AI edit with real transcript content
    let fixture = fixture_path("droid-session.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();
    fs::write(&file_path, "// initial\n// ai line one\n// ai line two\n").unwrap();
    repo.git_ai(&["checkpoint", "droid", "--hook-input", &hook_input])
        .unwrap();

    // Commit
    let commit = repo.stage_all_and_commit("Add AI lines").unwrap();

    // Should have exactly one prompt record
    assert_eq!(
        commit.authorship_log.metadata.prompts.len(),
        1,
        "Expected a single prompt record"
    );
    let prompt_record = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Prompt record should exist");

    // The latest checkpoint (with the real transcript) should win
    assert!(
        !prompt_record.messages.is_empty(),
        "Prompt record should contain messages from the latest checkpoint"
    );
    assert_eq!(
        prompt_record.agent_id.model, "custom:BYOK-GPT-5-MINI-0",
        "Prompt record should use the model from settings.json"
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

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input),
    };

    let preset = DroidPreset;
    let result = preset.run(flags).expect("Failed to run DroidPreset");

    assert_eq!(
        result.checkpoint_kind,
        CheckpointKind::Human,
        "PreToolUse should produce a Human checkpoint"
    );

    assert!(
        result.will_edit_filepaths.is_some(),
        "will_edit_filepaths should be populated for PreToolUse"
    );

    let will_edit = result.will_edit_filepaths.unwrap();
    assert_eq!(
        will_edit[0],
        "/Users/testuser/projects/testing-git/index.ts"
    );

    assert!(
        result.transcript.is_none(),
        "Transcript should be None for Human checkpoint"
    );
}

#[test]
fn test_droid_session_paths_derivation() {
    let (jsonl, settings) =
        DroidPreset::droid_session_paths("abc-123", "/Users/testuser/projects/my-app");
    assert!(
        jsonl.ends_with("-Users-testuser-projects-my-app/abc-123.jsonl"),
        "JSONL path should encode cwd with dashes, got: {:?}",
        jsonl
    );
    assert!(
        settings.ends_with("-Users-testuser-projects-my-app/abc-123.settings.json"),
        "Settings path should encode cwd with dashes, got: {:?}",
        settings
    );
}

#[test]
fn test_droid_settings_missing_model_field() {
    let mut temp = NamedTempFile::new().unwrap();
    temp.write_all(b"{}").unwrap();
    let result =
        DroidPreset::model_from_droid_settings_json(temp.path().to_str().unwrap()).unwrap();
    assert!(result.is_none(), "Missing model field should return None");
}

#[test]
fn test_droid_jsonl_parses_thinking_blocks() {
    let jsonl = r#"{"type":"message","id":"m1","timestamp":"2026-01-28T17:00:00.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Let me think about this..."},{"type":"text","text":"Here is my answer."}]}}
"#;
    let mut temp = NamedTempFile::new().unwrap();
    temp.write_all(jsonl.as_bytes()).unwrap();
    let (transcript, _) =
        DroidPreset::transcript_and_model_from_droid_jsonl(temp.path().to_str().unwrap()).unwrap();
    assert_eq!(
        transcript.messages().len(),
        2,
        "Should parse both thinking and text blocks"
    );
    // First should be thinking (parsed as Assistant)
    if let Message::Assistant { text, .. } = &transcript.messages()[0] {
        assert!(
            text.contains("think"),
            "First message should be the thinking block"
        );
    } else {
        panic!("Expected Assistant (thinking)");
    }
    // Second should be text
    assert!(
        matches!(transcript.messages()[1], Message::Assistant { .. }),
        "Second message should be Assistant text"
    );
}

reuse_tests_in_worktree!(
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
    test_droid_session_paths_derivation,
    test_droid_settings_missing_model_field,
    test_droid_jsonl_parses_thinking_blocks,
);
