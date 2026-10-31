use crate::test_utils::fixture_path;
use git_ai::authorship::authorship_log_serialization::generate_session_id;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use git_ai::metrics::attrs::attr_pos;
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::types::MetricEventId;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn parse_opencode(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("opencode")?.parse(hook_input, "t_test")
}

fn opencode_sqlite_fixture_path() -> std::path::PathBuf {
    fixture_path("opencode-sqlite")
}

#[test]
fn test_opencode_raw_event_fidelity() {
    use chrono::{DateTime, Utc};
    use git_ai::streams::agent::Agent;
    use git_ai::streams::agents::OpenCodeAgent;
    use git_ai::streams::watermark::TimestampWatermark;
    use rusqlite::OpenFlags;

    let opencode_root = opencode_sqlite_fixture_path();
    let fixture = opencode_root.join("opencode.db");
    let session_id = "test-session-123";

    let agent = OpenCodeAgent::new();
    let watermark = Box::new(TimestampWatermark::new(DateTime::<Utc>::UNIX_EPOCH));
    let result = agent
        .read_incremental(&fixture, watermark, session_id)
        .unwrap();

    // Independently query the SQLite DB to construct the same expected events.
    let conn = git_ai::sqlite::open_with_flags_and_memory_limits(
        &fixture,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();

    let watermark_millis = DateTime::<Utc>::UNIX_EPOCH.timestamp_millis();

    // Read full message rows for this session with time_updated > watermark (same filter as the agent)
    let mut msg_stmt = conn
        .prepare(
            "SELECT id, session_id, time_created, time_updated, data FROM message \
             WHERE session_id = ? AND time_updated > ? \
             ORDER BY time_updated ASC, id ASC",
        )
        .unwrap();
    let messages: Vec<(String, serde_json::Value)> = msg_stmt
        .query_map(rusqlite::params![session_id, watermark_millis], |row| {
            let id: String = row.get(0)?;
            let row_session_id: String = row.get(1)?;
            let time_created: i64 = row.get(2)?;
            let time_updated: i64 = row.get(3)?;
            let data: String = row.get(4)?;
            Ok((id, row_session_id, time_created, time_updated, data))
        })
        .unwrap()
        .map(|r| {
            let (id, row_session_id, time_created, time_updated, data) = r.unwrap();
            let parsed_data: serde_json::Value = serde_json::from_str(&data).unwrap();
            let row_json = json!({
                "id": id,
                "session_id": row_session_id,
                "time_created": time_created,
                "time_updated": time_updated,
                "data": parsed_data,
            });
            (id, row_json)
        })
        .collect();

    // Read parts only for matched messages via IN-subquery (same query as the agent)
    let mut part_stmt = conn
        .prepare(
            "SELECT id, message_id, session_id, time_created, time_updated, data FROM part \
             WHERE message_id IN ( \
                 SELECT id FROM message WHERE session_id = ? AND time_updated > ? \
             ) \
             ORDER BY message_id ASC, time_updated ASC, id ASC",
        )
        .unwrap();
    let parts_rows: Vec<(String, serde_json::Value)> = part_stmt
        .query_map(rusqlite::params![session_id, watermark_millis], |row| {
            let id: String = row.get(0)?;
            let message_id: String = row.get(1)?;
            let row_session_id: String = row.get(2)?;
            let time_created: i64 = row.get(3)?;
            let time_updated: i64 = row.get(4)?;
            let data: String = row.get(5)?;
            Ok((
                id,
                message_id,
                row_session_id,
                time_created,
                time_updated,
                data,
            ))
        })
        .unwrap()
        .map(|r| {
            let (id, message_id, row_session_id, time_created, time_updated, data) = r.unwrap();
            let parsed_data: serde_json::Value = serde_json::from_str(&data).unwrap();
            let row_json = json!({
                "id": id,
                "message_id": message_id,
                "session_id": row_session_id,
                "time_created": time_created,
                "time_updated": time_updated,
                "data": parsed_data,
            });
            (message_id, row_json)
        })
        .collect();

    let mut parts_by_msg: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    for (msg_id, row_json) in parts_rows {
        parts_by_msg.entry(msg_id).or_default().push(row_json);
    }

    let expected: Vec<serde_json::Value> = messages
        .iter()
        .map(|(id, row_json)| {
            if let Some(parts) = parts_by_msg.get(id) {
                json!({"message": row_json, "parts": parts})
            } else {
                json!({"message": row_json})
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
                e.stream_source.is_some(),
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

    let metrics_dir = tempfile::tempdir().unwrap();
    let metrics_db_path = metrics_dir.path().join("metrics.db");
    let mut repo = TestRepo::new_with_daemon_env(&[(
        "GIT_AI_TEST_METRICS_DB_PATH",
        metrics_db_path.to_str().unwrap(),
    )]);

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

    let session_id = generate_session_id("test-session-123", "opencode");
    repo.sync_daemon_force();
    let deadline = Instant::now() + Duration::from_secs(10);
    let emitted_session_event = loop {
        let metrics = MetricsDatabase::open_at_path(Path::new(&metrics_db_path)).unwrap();
        let session_events = metrics
            .get_metric_history(0, None, &[MetricEventId::SessionEvent as u16])
            .unwrap();
        if session_events.iter().any(|record| {
            record
                .event
                .attrs
                .get(&attr_pos::SESSION_ID.to_string())
                .and_then(|value| value.as_str())
                == Some(session_id.as_str())
        }) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        emitted_session_event,
        "OpenCode post-checkpoint should emit transcript events for its generated session ID"
    );

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

#[test]
fn test_opencode_transcript_ids_extracted_from_fixture() {
    use chrono::{DateTime, Utc};
    use git_ai::streams::agent::Agent;
    use git_ai::streams::agents::OpenCodeAgent;
    use git_ai::streams::watermark::TimestampWatermark;

    let fixture = fixture_path("opencode-sqlite/opencode.db");
    let agent = OpenCodeAgent::new();
    let watermark = Box::new(TimestampWatermark::new(DateTime::<Utc>::UNIX_EPOCH));
    let batch = agent
        .read_incremental(&fixture, watermark, "test-session-123")
        .unwrap();

    assert_eq!(batch.events.len(), 2, "Fixture has 2 messages");

    // Event 0: user message — has id, no parentID in data, no tool parts with callID
    let (eid, pid, tid) = agent.extract_event_ids(&batch.events[0]);
    assert_eq!(eid, Some("msg-user-sql-001".to_string()));
    assert_eq!(pid, None);
    assert_eq!(tid, None);

    // Event 1: assistant message — has id, parentID points to user msg, tool part has callID
    let (eid, pid, tid) = agent.extract_event_ids(&batch.events[1]);
    assert_eq!(eid, Some("msg-assistant-sql-001".to_string()));
    assert_eq!(pid, Some("msg-user-sql-001".to_string()));
    assert_eq!(tid, Some("call-sql-001".to_string()));
}

#[test]
fn test_opencode_tool_use_id_matches_hook_and_transcript() {
    use chrono::{DateTime, Utc};
    use git_ai::streams::agent::Agent;
    use git_ai::streams::agents::OpenCodeAgent;
    use git_ai::streams::watermark::TimestampWatermark;

    let fixture = fixture_path("opencode-sqlite/opencode.db");
    let agent = OpenCodeAgent::new();
    let watermark = Box::new(TimestampWatermark::new(DateTime::<Utc>::UNIX_EPOCH));
    let batch = agent
        .read_incremental(&fixture, watermark, "test-session-123")
        .unwrap();

    let assistant_event = &batch.events[1];
    let (_, _, tool_use_id_from_transcript) = agent.extract_event_ids(assistant_event);

    let hook_tool_use_id = "call-sql-001";
    assert_eq!(
        tool_use_id_from_transcript,
        Some(hook_tool_use_id.to_string()),
        "Transcript callID must match what the hook sends as tool_use_id"
    );
}

#[test]
#[serial_test::serial]
fn test_opencode_checkpoint_tool_use_id_matches_transcript_callid() {
    use crate::repos::test_repo::TestRepo;
    use chrono::{DateTime, Utc};
    use git_ai::streams::agent::Agent;
    use git_ai::streams::agents::OpenCodeAgent;
    use git_ai::streams::watermark::TimestampWatermark;

    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });

    let repo_root = repo.canonical_path();
    let file_path = repo_root.join("index.ts");
    std::fs::write(&file_path, "// initial\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let temp_storage = tempfile::tempdir().unwrap();
    let storage_path = temp_storage.path();
    let fixture_db = fixture_path("opencode-sqlite/opencode.db");
    std::fs::copy(&fixture_db, storage_path.join("opencode.db")).unwrap();

    unsafe {
        std::env::set_var(
            "GIT_AI_OPENCODE_STORAGE_PATH",
            storage_path.to_str().unwrap(),
        );
    }

    let pre_hook_input = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "test-session-123",
        "tool_use_id": "call-sql-001",
        "cwd": repo_root.to_string_lossy().to_string(),
        "tool_name": "edit",
        "tool_input": {
            "filePath": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    repo.git_ai(&["checkpoint", "opencode", "--hook-input", &pre_hook_input])
        .unwrap();

    std::fs::write(&file_path, "// initial\n// AI edit\n").unwrap();

    let post_hook_input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "test-session-123",
        "tool_use_id": "call-sql-001",
        "cwd": repo_root.to_string_lossy().to_string(),
        "tool_name": "edit",
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

    let agent = OpenCodeAgent::new();
    let watermark = Box::new(TimestampWatermark::new(DateTime::<Utc>::UNIX_EPOCH));
    let batch = agent
        .read_incremental(&fixture_db, watermark, "test-session-123")
        .unwrap();

    let (_, _, tid) = agent.extract_event_ids(&batch.events[1]);
    assert_eq!(
        tid,
        Some("call-sql-001".to_string()),
        "Transcript callID must equal the tool_use_id sent in the checkpoint hook"
    );
}

#[test]
#[serial_test::serial]
fn test_opencode_checkpoint_sets_parent_session_id_from_db() {
    let temp_storage = tempfile::tempdir().unwrap();
    let storage_path = temp_storage.path();
    let fixture_db = fixture_path("opencode-sqlite/opencode.db");
    std::fs::copy(&fixture_db, storage_path.join("opencode.db")).unwrap();

    unsafe {
        std::env::set_var(
            "GIT_AI_OPENCODE_STORAGE_PATH",
            storage_path.to_str().unwrap(),
        );
    }

    let hook_input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "test-session-123",
        "cwd": "/Users/test/project",
        "tool_name": "edit",
        "tool_use_id": "call-sql-001",
        "tool_input": {
            "filePath": "/Users/test/project/index.ts"
        }
    })
    .to_string();

    let preset = resolve_preset("opencode").unwrap();
    let events = preset.parse(&hook_input, "t_test").unwrap();

    unsafe {
        std::env::remove_var("GIT_AI_OPENCODE_STORAGE_PATH");
    }

    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            let ts = e
                .stream_source
                .as_ref()
                .expect("should have transcript source");
            assert_eq!(
                ts.external_parent_session_id,
                Some("parent-session-456".to_string()),
                "OpenCode checkpoint should look up parent_id from session table"
            );
            assert_eq!(ts.external_session_id, "test-session-123",);
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

crate::reuse_tests_in_worktree!(test_opencode_raw_event_fidelity,);

/// Exercise the standalone plugin template with the same Node 22 runtime used in CI.
/// The plugin's subprocesses inherit the TestRepo daemon/config isolation.
fn assert_opencode_plugin_callbacks(api: &str, scenario: &str) {
    use crate::repos::test_file::ExpectedLineExt;
    use crate::repos::test_repo::TestRepo;
    use std::process::Command;

    let metadata_target = scenario.starts_with("metadata");
    let raw_checkpoints = matches!(scenario, "untracked" | "failed-pre");
    let mut repo = if scenario == "worktree" {
        TestRepo::new_worktree()
    } else {
        TestRepo::new()
    };
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });
    let root = repo.canonical_path();
    let directory = if scenario == "subdir" {
        root.join("nested")
    } else {
        root.clone()
    };
    fs::create_dir_all(&directory).unwrap();
    for name in ["first.ts", "second.ts", "unrelated.ts"] {
        fs::write(directory.join(name), "// baseline\n").unwrap();
    }
    if scenario == "subdir" {
        fs::write(root.join("first.ts"), "// untouched root file\n").unwrap();
    }
    repo.stage_all_and_commit("Initial files").unwrap();
    let relative_name = |name: &str| {
        directory
            .join(name)
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    let mut first = repo.filename(&relative_name("first.ts"));
    let mut second = repo.filename(&relative_name("second.ts"));
    let mut unrelated = repo.filename(&relative_name("unrelated.ts"));
    first.assert_committed_lines(crate::lines!["// baseline".unattributed_human()]);
    second.assert_committed_lines(crate::lines!["// baseline".unattributed_human()]);
    unrelated.assert_committed_lines(crate::lines!["// baseline".unattributed_human()]);
    if scenario == "subdir" {
        repo.filename("first.ts")
            .assert_committed_lines(crate::lines!["// untouched root file".unattributed_human(),]);
    }

    // Exercise human edits on both sides of an AI edit in the same files.
    fs::write(directory.join("first.ts"), "// baseline\n// manual first\n").unwrap();
    fs::write(
        directory.join("second.ts"),
        "// baseline\n// manual second\n",
    )
    .unwrap();

    if !raw_checkpoints {
        let mut paths = vec![directory.join("first.ts")];
        if !metadata_target {
            paths.push(directory.join("second.ts"));
        }
        let mut args = vec!["checkpoint", "mock_known_human"];
        args.extend(paths.iter().map(|path| path.to_str().unwrap()));
        repo.git_ai(&args).unwrap();
    }

    let isolated = tempfile::tempdir().unwrap();
    let command_env = repo.git_ai_command_without_pre_sync_for_test(&[], &[]);
    let binary_path = command_env.get_program().to_str().unwrap();
    let escaped_binary = serde_json::to_string(binary_path).unwrap();
    let plugin_source = include_str!("../../agent-support/opencode/git-ai.ts")
        .replace("\"__GIT_AI_BINARY_PATH__\"", &escaped_binary);
    // .mts forces ESM without installing either OpenCode SDK alongside the plugin.
    let plugin_path = isolated.path().join("git-ai.mts");
    fs::write(&plugin_path, plugin_source).unwrap();
    let storage_path = isolated.path().join("storage");
    fs::create_dir(&storage_path).unwrap();
    fs::copy(
        opencode_sqlite_fixture_path().join("opencode.db"),
        storage_path.join("opencode.db"),
    )
    .unwrap();

    let mut command = Command::new("node");
    for (key, value) in command_env.get_envs() {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    let output = command
        .arg("--experimental-strip-types")
        .arg(fixture_path("opencode-plugin-harness.mjs"))
        .arg(&plugin_path)
        .arg(&directory)
        .arg(api)
        .arg(repo.base_repo_path().unwrap_or(&root))
        .arg(scenario)
        .arg(binary_path)
        .env("GIT_AI_OPENCODE_STORAGE_PATH", &storage_path)
        .env("GIT_AI_OPENCODE_DEBUG", "1")
        .current_dir(isolated.path())
        .output()
        .expect("OpenCode plugin tests require Node.js 22.6 or newer");
    assert!(
        output.status.success(),
        "Standalone {api} plugin failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    repo.sync_daemon_force();
    let checkpoints = repo.current_working_logs().read_all_checkpoints().unwrap();
    let ai_checkpoints: Vec<_> = checkpoints
        .iter()
        .filter(|checkpoint| {
            checkpoint.kind == git_ai::authorship::working_log::CheckpointKind::AiAgent
        })
        .collect();
    assert_eq!(
        ai_checkpoints.len(),
        if metadata_target || scenario == "failed-pre" {
            1
        } else {
            2
        },
        "Only completions with a successful pre-edit baseline may produce AI checkpoints"
    );
    let tool_ids: std::collections::BTreeSet<_> = ai_checkpoints
        .iter()
        .filter_map(|checkpoint| checkpoint.agent_metadata.as_ref()?.get("tool_use_id"))
        .map(String::as_str)
        .collect();
    let expected_tools = if metadata_target {
        std::collections::BTreeSet::from(["call-first"])
    } else if scenario == "failed-pre" {
        std::collections::BTreeSet::from(["call-second"])
    } else {
        std::collections::BTreeSet::from(["call-first", "call-second"])
    };
    assert_eq!(tool_ids, expected_tools);
    if metadata_target {
        assert!(
            ai_checkpoints
                .iter()
                .flat_map(|checkpoint| &checkpoint.entries)
                .all(|entry| entry.file == relative_name("first.ts")),
            "Result metadata must not expand attribution to a file absent from the pre snapshot"
        );
    }
    if raw_checkpoints {
        // Commit-time adjacent-edge recovery intentionally extends AI attribution into
        // nearby unknown lines. Check the raw hook snapshots before that separate policy.
        for checkpoint in ai_checkpoints {
            for entry in &checkpoint.entries {
                assert_eq!(entry.line_attributions.len(), 1);
                assert_eq!(entry.line_attributions[0].start_line, 3);
                assert_eq!(entry.line_attributions[0].end_line, 3);
            }
        }
        return;
    }
    let commit = repo.stage_all_and_commit("OpenCode plugin edits").unwrap();
    first.assert_committed_lines(crate::lines![
        "// baseline".unattributed_human(),
        "// manual first".human(),
        "// AI first".ai(),
        "// manual after".human(),
    ]);
    if metadata_target {
        second.assert_committed_lines(crate::lines![
            "// baseline".unattributed_human(),
            "// manual second".unattributed_human(),
            "// AI second".unattributed_human(),
        ]);
    } else {
        second.assert_committed_lines(crate::lines![
            "// baseline".unattributed_human(),
            "// manual second".human(),
            "// AI second".ai(),
        ]);
    }
    unrelated.assert_committed_lines(crate::lines![
        "// baseline".unattributed_human(),
        "// unrelated manual".unattributed_human(),
    ]);
    if scenario == "subdir" {
        repo.filename("first.ts")
            .assert_committed_lines(crate::lines!["// untouched root file".unattributed_human(),]);
    }
    let session_id = generate_session_id("test-session-123", "opencode");
    let session = commit
        .authorship_log
        .metadata
        .sessions
        .get(&session_id)
        .unwrap();
    assert_eq!(session.agent_id.tool, "opencode");
    assert_eq!(session.agent_id.id, "test-session-123");
    assert_eq!(session.agent_id.model, "gpt-5");
}

#[test]
fn test_opencode_plugin_v2_callbacks_attribute_edits() {
    assert_opencode_plugin_callbacks("v2", "default");
}

#[test]
fn test_opencode_plugin_v1_callbacks_attribute_edits() {
    assert_opencode_plugin_callbacks("v1", "default");
}

#[test]
fn test_opencode_plugin_v2_uses_active_worktree_directory() {
    assert_opencode_plugin_callbacks("v2", "worktree");
}

#[test]
fn test_opencode_plugin_v2_uses_active_subdirectory() {
    assert_opencode_plugin_callbacks("v2", "subdir");
}

#[test]
fn test_opencode_plugin_v2_skips_metadata_targets_without_pre_snapshot() {
    assert_opencode_plugin_callbacks("v2", "metadata");
}

#[test]
fn test_opencode_plugin_v2_pre_snapshot_excludes_untracked_edits() {
    assert_opencode_plugin_callbacks("v2", "untracked");
}

#[test]
fn test_opencode_plugin_v2_metadata_cannot_expand_pre_snapshot_scope() {
    assert_opencode_plugin_callbacks("v2", "metadata-extra");
}

#[test]
fn test_opencode_plugin_v2_failed_pre_does_not_attribute_post() {
    assert_opencode_plugin_callbacks("v2", "failed-pre");
}
