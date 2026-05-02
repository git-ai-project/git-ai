use crate::test_utils::fixture_path;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

fn parse_opencode(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("opencode")?.parse(hook_input, "t_test")
}

fn opencode_sqlite_fixture_path() -> std::path::PathBuf {
    fixture_path("opencode-sqlite")
}

#[test]
fn test_opencode_raw_event_fidelity() {
    use chrono::{DateTime, Utc};
    use git_ai::transcripts::agent::Agent;
    use git_ai::transcripts::agents::OpenCodeAgent;
    use git_ai::transcripts::watermark::TimestampWatermark;
    use rusqlite::{Connection, OpenFlags};

    let opencode_root = opencode_sqlite_fixture_path();
    let fixture = opencode_root.join("opencode.db");
    let session_id = "test-session-123";

    let agent = OpenCodeAgent;
    let watermark = Box::new(TimestampWatermark::new(DateTime::<Utc>::UNIX_EPOCH));
    let result = agent
        .read_incremental(&fixture, watermark, session_id)
        .unwrap();

    // Independently query the SQLite DB to construct the same expected events.
    let conn = Connection::open_with_flags(&fixture, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();

    let watermark_millis = DateTime::<Utc>::UNIX_EPOCH.timestamp_millis();

    // Read messages for this session with time_created > watermark (same filter as the agent)
    let mut msg_stmt = conn
        .prepare(
            "SELECT id, time_created, data FROM message \
             WHERE session_id = ? AND time_created > ? \
             ORDER BY time_created ASC, id ASC",
        )
        .unwrap();
    let messages: Vec<(String, i64, serde_json::Value)> = msg_stmt
        .query_map(rusqlite::params![session_id, watermark_millis], |row| {
            let id: String = row.get(0)?;
            let time_created: i64 = row.get(1)?;
            let data: String = row.get(2)?;
            Ok((id, time_created, data))
        })
        .unwrap()
        .map(|r| {
            let (id, time_created, data) = r.unwrap();
            (id, time_created, serde_json::from_str(&data).unwrap())
        })
        .collect();

    // Read parts grouped by message_id (same query as the agent)
    let mut part_stmt = conn
        .prepare(
            "SELECT message_id, data FROM part \
             WHERE session_id = ? \
             ORDER BY message_id ASC, time_created ASC, id ASC",
        )
        .unwrap();
    let parts_rows: Vec<(String, serde_json::Value)> = part_stmt
        .query_map(rusqlite::params![session_id], |row| {
            let message_id: String = row.get(0)?;
            let data: String = row.get(1)?;
            Ok((message_id, data))
        })
        .unwrap()
        .map(|r| {
            let (message_id, data) = r.unwrap();
            (message_id, serde_json::from_str(&data).unwrap())
        })
        .collect();

    let mut parts_by_msg: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    for (msg_id, data) in parts_rows {
        parts_by_msg.entry(msg_id).or_default().push(data);
    }

    let expected: Vec<serde_json::Value> = messages
        .iter()
        .map(|(id, _, data)| {
            if let Some(parts) = parts_by_msg.get(id) {
                serde_json::json!({"message": data, "parts": parts})
            } else {
                serde_json::json!({"message": data})
            }
        })
        .collect();

    assert_eq!(result.events.len(), expected.len());
    assert_eq!(result.events, expected);
}

#[test]
#[serial_test::serial]
fn test_opencode_preset_pretooluse_returns_human_checkpoint() {
    let storage_path = opencode_sqlite_fixture_path();

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
    let storage_path = opencode_sqlite_fixture_path();

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
            // Model is extracted from the OpenCode SQLite fixture at parse time
            assert_eq!(e.context.agent_id.model, "gpt-5");
        }
        _ => panic!("Expected PostFileEdit for PostToolUse"),
    }
}

#[test]
#[serial_test::serial]
fn test_opencode_preset_stores_session_id_in_metadata() {
    let storage_path = opencode_sqlite_fixture_path();

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
    let storage_path = opencode_sqlite_fixture_path();

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
#[serial_test::serial]
fn test_opencode_preset_extracts_apply_patch_paths() {
    let storage_path = opencode_sqlite_fixture_path();

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

    // Copy the sqlite fixture's opencode.db to the temp storage directory
    let fixture_db = opencode_sqlite_fixture_path().join("opencode.db");
    fs::copy(&fixture_db, storage_path.join("opencode.db")).unwrap();

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
        session_record.agent_id.model, "gpt-5",
        "Session record model should be extracted from OpenCode SQLite fixture"
    );
}

crate::reuse_tests_in_worktree!(test_opencode_raw_event_fidelity,);
