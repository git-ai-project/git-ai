use crate::test_utils::fixture_path;
use git_ai::authorship::transcript::Message;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::commands::checkpoint_agent::transcript_readers;
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

fn parse_opencode(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("opencode")?.parse(hook_input, "t_test")
}

fn opencode_storage_fixture_path() -> std::path::PathBuf {
    fixture_path("opencode-storage")
}

fn opencode_sqlite_fixture_path() -> std::path::PathBuf {
    fixture_path("opencode-sqlite")
}

fn opencode_sqlite_empty_fixture_path() -> std::path::PathBuf {
    fixture_path("opencode-sqlite-empty")
}

#[test]
fn test_parse_opencode_storage_transcript() {
    let storage_path = opencode_storage_fixture_path();
    let session_id = "test-session-123";

    let (transcript, model) =
        transcript_readers::read_opencode_from_storage(&storage_path, session_id)
            .expect("Failed to parse OpenCode storage");

    assert!(
        !transcript.messages().is_empty(),
        "Transcript should contain messages"
    );

    assert!(
        model.is_some(),
        "Model should be extracted from assistant message"
    );
    assert_eq!(
        model.unwrap(),
        "anthropic/claude-3-5-sonnet-20241022",
        "Model should be provider/model format"
    );

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
fn test_parse_opencode_sqlite_transcript() {
    let opencode_root = opencode_sqlite_fixture_path();
    let session_id = "test-session-123";

    let (transcript, model) =
        transcript_readers::read_opencode_from_storage(&opencode_root, session_id)
            .expect("Failed to parse OpenCode sqlite storage");

    assert!(
        !transcript.messages().is_empty(),
        "Transcript should contain messages"
    );
    assert_eq!(
        model.as_deref(),
        Some("openai/gpt-5"),
        "Model should come from sqlite assistant message metadata"
    );

    assert!(
        matches!(transcript.messages()[0], Message::User { .. }),
        "First message should be from user"
    );
    if let Message::User { text, .. } = &transcript.messages()[0] {
        assert!(
            text.contains("sqlite transcript data"),
            "Expected sqlite fixture user text"
        );
    }
}

#[test]
fn test_opencode_sqlite_takes_precedence_over_legacy_storage() {
    let temp_dir = tempfile::tempdir().unwrap();
    let opencode_root = temp_dir.path();

    let sqlite_db = opencode_sqlite_fixture_path().join("opencode.db");
    fs::copy(&sqlite_db, opencode_root.join("opencode.db")).unwrap();

    let legacy_storage = opencode_storage_fixture_path();
    copy_dir_all(&legacy_storage, &opencode_root.join("storage")).unwrap();

    let (transcript, model) =
        transcript_readers::read_opencode_from_storage(opencode_root, "test-session-123")
            .expect("Should parse from sqlite first");

    assert_eq!(model.as_deref(), Some("openai/gpt-5"));
    if let Message::User { text, .. } = &transcript.messages()[0] {
        assert!(
            text.contains("sqlite transcript data"),
            "sqlite transcript should win over legacy storage"
        );
        assert!(
            !text.contains("Update the comment"),
            "legacy transcript should not be used when sqlite has data"
        );
    }
}

#[test]
fn test_opencode_sqlite_falls_back_to_legacy_storage_when_sqlite_empty() {
    let temp_dir = tempfile::tempdir().unwrap();
    let opencode_root = temp_dir.path();

    let sqlite_db = opencode_sqlite_empty_fixture_path().join("opencode.db");
    fs::copy(&sqlite_db, opencode_root.join("opencode.db")).unwrap();

    let legacy_storage = opencode_storage_fixture_path();
    copy_dir_all(&legacy_storage, &opencode_root.join("storage")).unwrap();

    let (transcript, model) =
        transcript_readers::read_opencode_from_storage(opencode_root, "test-session-123")
            .expect("Should fallback to legacy storage when sqlite has no session data");

    assert_eq!(
        model.as_deref(),
        Some("anthropic/claude-3-5-sonnet-20241022")
    );
    if let Message::User { text, .. } = &transcript.messages()[0] {
        assert!(
            text.contains("Update the comment"),
            "Should fallback to legacy fixture transcript"
        );
    }
}

#[test]
fn test_opencode_transcript_message_order() {
    let storage_path = opencode_storage_fixture_path();
    let session_id = "test-session-123";

    let (transcript, _) = transcript_readers::read_opencode_from_storage(&storage_path, session_id)
        .expect("Failed to parse OpenCode storage");

    assert!(
        matches!(transcript.messages()[0], Message::User { .. }),
        "First message should be from user"
    );

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

    let (transcript, _) = transcript_readers::read_opencode_from_storage(&storage_path, session_id)
        .expect("Failed to parse OpenCode storage");

    for message in transcript.messages() {
        match message {
            Message::User { timestamp, .. }
            | Message::Assistant { timestamp, .. }
            | Message::ToolUse { timestamp, .. }
            | Message::Thinking { timestamp, .. }
            | Message::Plan { timestamp, .. } => {
                if let Some(ts) = timestamp {
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
#[serial_test::serial]
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

    unsafe {
        std::env::set_var(
            "GIT_AI_OPENCODE_STORAGE_PATH",
            storage_path.to_str().unwrap(),
        );
    }

    let events = parse_opencode(&hook_input).expect("Failed to run OpenCodePreset");

    unsafe {
        std::env::remove_var("GIT_AI_OPENCODE_STORAGE_PATH");
    }

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.context.cwd, PathBuf::from("/Users/test/project"));
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
#[serial_test::serial]
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

    unsafe {
        std::env::set_var(
            "GIT_AI_OPENCODE_STORAGE_PATH",
            storage_path.to_str().unwrap(),
        );
    }

    let events = parse_opencode(&hook_input).expect("Failed to run OpenCodePreset");

    unsafe {
        std::env::remove_var("GIT_AI_OPENCODE_STORAGE_PATH");
    }

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(
                e.transcript_source.is_some(),
                "Transcript should be present for AI checkpoint"
            );
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("index.ts")),
                "edited_filepaths should contain the target file"
            );
            assert_eq!(e.context.agent_id.tool, "opencode");
            assert_eq!(e.context.agent_id.id, "test-session-123");
            // Model is lazily resolved from transcript, so at parse time it's "unknown"
            assert_eq!(e.context.agent_id.model, "unknown");
        }
        _ => panic!("Expected PostFileEdit for PostToolUse"),
    }
}

#[test]
#[serial_test::serial]
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

    let events = parse_opencode(&hook_input).expect("Failed to run OpenCodePreset");

    unsafe {
        std::env::remove_var("GIT_AI_OPENCODE_STORAGE_PATH");
    }

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(
                e.context.metadata.contains_key("session_id"),
                "Metadata should contain session_id"
            );
            assert_eq!(e.context.metadata["session_id"], "test-session-123");
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
#[serial_test::serial]
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

    let events = parse_opencode(&hook_input).expect("Failed to run OpenCodePreset");

    unsafe {
        std::env::remove_var("GIT_AI_OPENCODE_STORAGE_PATH");
    }

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.cwd, PathBuf::from("/Users/test/my-project"));
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_opencode_empty_session_returns_empty_transcript() {
    let temp_dir = tempfile::tempdir().unwrap();
    let storage_path = temp_dir.path();
    let session_id = "empty-session";

    let message_dir = storage_path.join("message").join(session_id);
    fs::create_dir_all(&message_dir).unwrap();

    let (transcript, model) =
        transcript_readers::read_opencode_from_storage(storage_path, session_id)
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
        transcript_readers::read_opencode_from_storage(&storage_path, session_id)
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

    let (transcript, _) = transcript_readers::read_opencode_from_storage(&storage_path, session_id)
        .expect("Failed to parse OpenCode storage");

    let tool_uses: Vec<_> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::ToolUse { .. }))
        .collect();

    assert!(!tool_uses.is_empty(), "Should have tool use messages");

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
#[serial_test::serial]
fn test_opencode_preset_extracts_apply_patch_paths() {
    let storage_path = opencode_storage_fixture_path();

    let patch_text = "*** Begin Patch\n*** Update File: src/main.ts\n@@\n-old\n+new\n*** End Patch";
    let hook_input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "test-session-123",
        "cwd": "/Users/test/my-project",
        "tool_name": "apply_patch",
        "tool_input": {
            "patchText": patch_text
        }
    })
    .to_string();

    unsafe {
        std::env::set_var(
            "GIT_AI_OPENCODE_STORAGE_PATH",
            storage_path.to_str().unwrap(),
        );
    }

    let events = parse_opencode(&hook_input).expect("Failed to run OpenCodePreset");

    unsafe {
        std::env::remove_var("GIT_AI_OPENCODE_STORAGE_PATH");
    }

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            let path_strs: Vec<String> = e
                .file_paths
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            assert!(
                path_strs.iter().any(|p| p.contains("src/main.ts")),
                "Should extract file paths from apply_patch, got: {:?}",
                path_strs
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
#[serial_test::serial]
fn test_opencode_e2e_checkpoint_and_commit() {
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

    let temp_storage = tempfile::tempdir().unwrap();
    let storage_path = temp_storage.path();

    let fixture_storage = opencode_storage_fixture_path();
    copy_dir_all(&fixture_storage, storage_path).unwrap();

    unsafe {
        std::env::set_var(
            "GIT_AI_OPENCODE_STORAGE_PATH",
            storage_path.to_str().unwrap(),
        );
    }

    let pre_hook_input = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "test-session-123",
        "cwd": repo_root.to_string_lossy().to_string(),
        "tool_input": {
            "filePath": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    repo.git_ai(&["checkpoint", "opencode", "--hook-input", &pre_hook_input])
        .unwrap();

    fs::write(&file_path, "// initial\n// Hello World\n").unwrap();

    let post_hook_input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "test-session-123",
        "cwd": repo_root.to_string_lossy().to_string(),
        "tool_input": {
            "filePath": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    repo.git_ai(&["checkpoint", "opencode", "--hook-input", &post_hook_input])
        .unwrap();

    unsafe {
        std::env::remove_var("GIT_AI_OPENCODE_STORAGE_PATH");
    }

    let commit = repo.stage_all_and_commit("Add AI line").unwrap();

    assert!(
        !commit.authorship_log.metadata.sessions.is_empty(),
        "Should have at least one session record"
    );

    let session_record = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("Session record should exist");

    assert_eq!(
        session_record.agent_id.tool, "opencode",
        "Agent tool should be opencode"
    );
    assert_eq!(
        session_record.agent_id.model, "anthropic/claude-3-5-sonnet-20241022",
        "Model should match fixture"
    );
}

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

crate::reuse_tests_in_worktree!(
    test_parse_opencode_storage_transcript,
    test_parse_opencode_sqlite_transcript,
    test_opencode_sqlite_takes_precedence_over_legacy_storage,
    test_opencode_sqlite_falls_back_to_legacy_storage_when_sqlite_empty,
    test_opencode_transcript_message_order,
    test_opencode_transcript_timestamps_are_rfc3339,
    test_opencode_empty_session_returns_empty_transcript,
    test_opencode_nonexistent_session_returns_empty_transcript,
    test_opencode_tool_use_only_from_assistant,
);
