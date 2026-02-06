#[macro_use]
mod repos;
mod test_utils;

use git_ai::authorship::transcript::Message;
use git_ai::authorship::working_log::CheckpointKind;
use git_ai::commands::checkpoint_agent::agent_presets::{AgentCheckpointFlags, AgentCheckpointPreset};
use git_ai::commands::checkpoint_agent::opencode_preset::OpenCodePreset;
use serde_json::json;
use std::fs;
use test_utils::fixture_path;

fn opencode_storage_fixture_path() -> std::path::PathBuf {
    fixture_path("opencode-storage")
}

#[test]
fn test_parse_opencode_storage_transcript() {
    let storage_path = opencode_storage_fixture_path();
    let session_id = "test-session-123";

    let (transcript, model) =
        OpenCodePreset::transcript_and_model_from_storage(&storage_path, session_id)
            .expect("Failed to parse OpenCode storage");

    // Verify we parsed messages
    assert!(
        !transcript.messages().is_empty(),
        "Transcript should contain messages"
    );

    // Model should be extracted from first assistant message
    assert!(model.is_some(), "Model should be extracted from assistant message");
    assert_eq!(
        model.unwrap(),
        "anthropic/claude-3-5-sonnet-20241022",
        "Model should be provider/model format"
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
}

#[test]
fn test_opencode_transcript_message_order() {
    let storage_path = opencode_storage_fixture_path();
    let session_id = "test-session-123";

    let (transcript, _) =
        OpenCodePreset::transcript_and_model_from_storage(&storage_path, session_id)
            .expect("Failed to parse OpenCode storage");

    // Messages should be sorted by creation time
    // User message comes first (earlier timestamp), then assistant message
    assert!(
        matches!(transcript.messages()[0], Message::User { .. }),
        "First message should be from user"
    );

    // Verify user message text
    if let Message::User { text, .. } = &transcript.messages()[0] {
        assert!(
            text.contains("Update the comment"),
            "User message should contain expected text"
        );
    }
}

#[test]
fn test_opencode_transcript_timestamps_are_rfc3339() {
    let storage_path = opencode_storage_fixture_path();
    let session_id = "test-session-123";

    let (transcript, _) =
        OpenCodePreset::transcript_and_model_from_storage(&storage_path, session_id)
            .expect("Failed to parse OpenCode storage");

    for message in transcript.messages() {
        match message {
            Message::User { timestamp, .. }
            | Message::Assistant { timestamp, .. }
            | Message::ToolUse { timestamp, .. }
            | Message::Thinking { timestamp, .. }
            | Message::Plan { timestamp, .. } => {
                if let Some(ts) = timestamp {
                    // RFC3339 format: 2024-01-28T15:30:30+00:00 or similar
                    assert!(
                        ts.contains("T") && (ts.contains("+") || ts.ends_with("Z")),
                        "Timestamp should be RFC3339 format, got: {}",
                        ts
                    );
                }
            }
        }
    }
}

#[test]
fn test_opencode_preset_pretooluse_returns_human_checkpoint() {
    let storage_path = opencode_storage_fixture_path();

    let hook_input = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "test-session-123",
        "cwd": "/Users/test/project",
        "tool_input": {
            "filePath": "/Users/test/project/index.ts"
        }
    })
    .to_string();

    // Set the test storage path via env var
    unsafe {
        std::env::set_var(
            "GIT_AI_OPENCODE_STORAGE_PATH",
            storage_path.to_str().unwrap(),
        );
    }

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input),
    };

    let result = OpenCodePreset.run(flags).expect("Failed to run OpenCodePreset");

    // Clean up env var
    unsafe {
        std::env::remove_var("GIT_AI_OPENCODE_STORAGE_PATH");
    }

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
    assert_eq!(will_edit[0], "/Users/test/project/index.ts");

    assert!(
        result.transcript.is_none(),
        "Transcript should be None for Human checkpoint"
    );
}

#[test]
fn test_opencode_preset_posttooluse_returns_ai_checkpoint() {
    let storage_path = opencode_storage_fixture_path();

    let hook_input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "test-session-123",
        "cwd": "/Users/test/project",
        "tool_input": {
            "filePath": "/Users/test/project/index.ts"
        }
    })
    .to_string();

    // Set the test storage path via env var
    unsafe {
        std::env::set_var(
            "GIT_AI_OPENCODE_STORAGE_PATH",
            storage_path.to_str().unwrap(),
        );
    }

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input),
    };

    let result = OpenCodePreset.run(flags).expect("Failed to run OpenCodePreset");

    // Clean up env var
    unsafe {
        std::env::remove_var("GIT_AI_OPENCODE_STORAGE_PATH");
    }

    assert_eq!(
        result.checkpoint_kind,
        CheckpointKind::AiAgent,
        "PostToolUse should produce an AiAgent checkpoint"
    );

    assert!(
        result.transcript.is_some(),
        "Transcript should be present for AI checkpoint"
    );

    assert!(
        result.edited_filepaths.is_some(),
        "edited_filepaths should be populated for PostToolUse"
    );

    let edited = result.edited_filepaths.unwrap();
    assert_eq!(edited[0], "/Users/test/project/index.ts");

    assert_eq!(result.agent_id.tool, "opencode");
    assert_eq!(result.agent_id.id, "test-session-123");
    assert_eq!(result.agent_id.model, "anthropic/claude-3-5-sonnet-20241022");
}

#[test]
fn test_opencode_preset_stores_session_id_in_metadata() {
    let storage_path = opencode_storage_fixture_path();

    let hook_input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "test-session-123",
        "cwd": "/Users/test/project",
        "tool_input": {
            "filePath": "/Users/test/project/index.ts"
        }
    })
    .to_string();

    unsafe {
        std::env::set_var(
            "GIT_AI_OPENCODE_STORAGE_PATH",
            storage_path.to_str().unwrap(),
        );
    }

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input),
    };

    let result = OpenCodePreset.run(flags).expect("Failed to run OpenCodePreset");

    unsafe {
        std::env::remove_var("GIT_AI_OPENCODE_STORAGE_PATH");
    }

    assert!(result.agent_metadata.is_some());
    let metadata = result.agent_metadata.unwrap();
    assert!(
        metadata.contains_key("session_id"),
        "Metadata should contain session_id"
    );
    assert_eq!(metadata["session_id"], "test-session-123");
}

#[test]
fn test_opencode_preset_sets_repo_working_dir() {
    let storage_path = opencode_storage_fixture_path();

    let hook_input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "test-session-123",
        "cwd": "/Users/test/my-project",
        "tool_input": {
            "filePath": "/Users/test/my-project/src/main.ts"
        }
    })
    .to_string();

    unsafe {
        std::env::set_var(
            "GIT_AI_OPENCODE_STORAGE_PATH",
            storage_path.to_str().unwrap(),
        );
    }

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input),
    };

    let result = OpenCodePreset.run(flags).expect("Failed to run OpenCodePreset");

    unsafe {
        std::env::remove_var("GIT_AI_OPENCODE_STORAGE_PATH");
    }

    assert!(result.repo_working_dir.is_some());
    assert_eq!(
        result.repo_working_dir.unwrap(),
        "/Users/test/my-project"
    );
}

#[test]
fn test_opencode_empty_session_returns_empty_transcript() {
    // Create a temp directory with empty session structure
    let temp_dir = tempfile::tempdir().unwrap();
    let storage_path = temp_dir.path();
    let session_id = "empty-session";

    // Create message directory but no files
    let message_dir = storage_path.join("message").join(session_id);
    fs::create_dir_all(&message_dir).unwrap();

    let (transcript, model) =
        OpenCodePreset::transcript_and_model_from_storage(storage_path, session_id)
            .expect("Should handle empty session");

    assert!(
        transcript.messages().is_empty(),
        "Empty session should produce empty transcript"
    );
    assert!(model.is_none(), "Empty session should have no model");
}

#[test]
fn test_opencode_nonexistent_session_returns_empty_transcript() {
    let storage_path = opencode_storage_fixture_path();
    let session_id = "nonexistent-session";

    let (transcript, model) =
        OpenCodePreset::transcript_and_model_from_storage(&storage_path, session_id)
            .expect("Should handle nonexistent session");

    assert!(
        transcript.messages().is_empty(),
        "Nonexistent session should produce empty transcript"
    );
    assert!(model.is_none(), "Nonexistent session should have no model");
}

#[test]
fn test_opencode_tool_use_only_from_assistant() {
    let storage_path = opencode_storage_fixture_path();
    let session_id = "test-session-123";

    let (transcript, _) =
        OpenCodePreset::transcript_and_model_from_storage(&storage_path, session_id)
            .expect("Failed to parse OpenCode storage");

    // Find tool use messages
    let tool_uses: Vec<_> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::ToolUse { .. }))
        .collect();

    assert!(!tool_uses.is_empty(), "Should have tool use messages");

    // Verify tool use has expected content
    if let Message::ToolUse { name, input, .. } = tool_uses[0] {
        assert_eq!(name, "edit", "Tool name should be 'edit'");
        assert!(
            input.get("filePath").is_some(),
            "Tool input should contain filePath"
        );
    } else {
        panic!("Expected ToolUse message");
    }
}

#[test]
fn test_opencode_e2e_checkpoint_and_commit() {
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

    // Copy fixture storage to temp location
    let temp_storage = tempfile::tempdir().unwrap();
    let storage_path = temp_storage.path();

    // Copy the fixture storage structure
    let fixture_storage = opencode_storage_fixture_path();
    copy_dir_all(&fixture_storage, storage_path).unwrap();

    // Set up environment for the test
    unsafe {
        std::env::set_var("GIT_AI_OPENCODE_STORAGE_PATH", storage_path.to_str().unwrap());
    }

    // Create hook input for PreToolUse (human checkpoint)
    let pre_hook_input = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "test-session-123",
        "cwd": repo_root.to_string_lossy().to_string(),
        "tool_input": {
            "filePath": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    // Run human checkpoint
    repo.git_ai(&["checkpoint", "opencode", "--hook-input", &pre_hook_input])
        .unwrap();

    // Make AI edit
    fs::write(&file_path, "// initial\n// Hello World\n").unwrap();

    // Create hook input for PostToolUse (AI checkpoint)
    let post_hook_input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "test-session-123",
        "cwd": repo_root.to_string_lossy().to_string(),
        "tool_input": {
            "filePath": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    // Run AI checkpoint
    repo.git_ai(&["checkpoint", "opencode", "--hook-input", &post_hook_input])
        .unwrap();

    // Clean up env var
    unsafe {
        std::env::remove_var("GIT_AI_OPENCODE_STORAGE_PATH");
    }

    // Commit
    let commit = repo.stage_all_and_commit("Add AI line").unwrap();

    // Should have a prompt record
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Should have at least one prompt record"
    );

    let prompt_record = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Prompt record should exist");

    assert_eq!(
        prompt_record.agent_id.tool, "opencode",
        "Agent tool should be opencode"
    );
    assert_eq!(
        prompt_record.agent_id.model, "anthropic/claude-3-5-sonnet-20241022",
        "Model should match fixture"
    );
}

/// Helper function to recursively copy a directory
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}
