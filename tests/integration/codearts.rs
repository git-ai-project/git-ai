use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use crate::test_utils::{fixture_path, isolated_metrics_db_path};
use git_ai::authorship::authorship_log_serialization::generate_session_id;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, StreamFormat, resolve_preset};
use git_ai::error::GitAiError;
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::types::MetricEventId;
use git_ai::metrics::{EventAttributes, PosEncoded, SessionEventValues};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

fn hook_input(event: &str, tool: &str, input: Value) -> Value {
    json!({
        "hook_event_name": event,
        "session_id": "codearts-session",
        "cwd": "/project",
        "tool_name": tool,
        "tool_use_id": "tool-call-1",
        "tool_input": input,
    })
}

fn parse_codearts(input: &Value) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("codearts")?.parse(&input.to_string(), "codearts-trace")
}

#[test]
fn test_codearts_pre_edit_preserves_session_and_invocation_identity() {
    let events = parse_codearts(&hook_input(
        "PreToolUse",
        "edit",
        json!({"filePath": "src/main.rs"}),
    ))
    .unwrap();
    assert_eq!(events.len(), 1);
    let ParsedHookEvent::PreFileEdit(event) = &events[0] else {
        panic!("expected a pre-edit event");
    };
    assert_eq!(
        event.file_paths,
        vec![PathBuf::from("/project/src/main.rs")]
    );
    assert_eq!(event.tool_use_id.as_deref(), Some("tool-call-1"));
    assert_eq!(event.context.agent_id.tool, "codearts");
    assert_eq!(event.context.agent_id.id, "codearts-session");
    assert_eq!(event.context.agent_id.model, "unknown");
    assert_eq!(event.context.external_session_id, "codearts-session");
    assert_eq!(event.context.trace_id, "codearts-trace");
    assert_eq!(event.context.cwd, PathBuf::from("/project"));
    assert_eq!(event.context.metadata["session_id"], "codearts-session");
}

#[test]
fn test_codearts_post_edit_uses_supplied_model_without_opencode_transcript() {
    let mut input = hook_input("PostToolUse", "write", json!({"filePath": "new.rs"}));
    input["model"] = json!("deepseek-v3.2");
    let events = parse_codearts(&input).unwrap();
    assert_eq!(events.len(), 1);
    let ParsedHookEvent::PostFileEdit(event) = &events[0] else {
        panic!("expected a post-edit event");
    };
    assert_eq!(event.context.agent_id.tool, "codearts");
    assert_eq!(event.context.agent_id.model, "deepseek-v3.2");
    assert_eq!(event.file_paths, vec![PathBuf::from("/project/new.rs")]);
    assert_eq!(event.tool_use_id.as_deref(), Some("tool-call-1"));
    assert!(event.stream_source.is_none());
}

#[test]
fn test_codearts_post_hooks_resolve_explicit_transcript_and_parent_session() {
    let storage = tempfile::tempdir().unwrap();
    // CodeArts uses OpenCode's SQLite schema, but its database may have any name.
    let database = storage.path().join("codearts.db");
    fs::copy(fixture_path("opencode-sqlite/opencode.db"), &database).unwrap();

    for tool in ["write", "bash", "shell"] {
        let mut input = hook_input(
            "PostToolUse",
            tool,
            json!({"filePath": "new.rs", "command": "echo generated > new.rs"}),
        );
        input["session_id"] = json!("test-session-123");
        input["transcript_path"] = json!(database);
        let events = parse_codearts(&input).unwrap();
        let source = match &events[0] {
            ParsedHookEvent::PostFileEdit(event) => &event.stream_source,
            ParsedHookEvent::PostBashCall(event) => &event.stream_source,
            _ => panic!("expected a post event for {tool}"),
        }
        .as_ref()
        .expect("CodeArts post hooks must register the supplied transcript");
        assert_eq!(source.path, database);
        assert_eq!(source.format, StreamFormat::OpenCodeSqlite);
        assert_eq!(
            source.session_id,
            generate_session_id("test-session-123", "codearts")
        );
        assert_eq!(source.external_session_id, "test-session-123");
        assert_eq!(
            source.external_parent_session_id.as_deref(),
            Some("parent-session-456")
        );
    }
}

#[test]
fn test_codearts_unavailable_transcript_does_not_block_attribution() {
    let storage = tempfile::tempdir().unwrap();
    for path in [
        Value::Null,
        json!(""),
        json!("opencode.db"),
        json!(storage.path()),
        json!(storage.path().join("missing.db")),
    ] {
        let mut input = hook_input("PostToolUse", "edit", json!({"filePath": "main.rs"}));
        input["transcript_path"] = path;
        let events = parse_codearts(&input).unwrap();
        let ParsedHookEvent::PostFileEdit(event) = &events[0] else {
            panic!("expected a post-edit event");
        };
        assert!(event.stream_source.is_none());
        assert_eq!(event.file_paths, vec![PathBuf::from("/project/main.rs")]);
    }
}

#[test]
fn test_codearts_session_update_respects_repository_filters() {
    let (_metrics_dir, metrics_path) = isolated_metrics_db_path();
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_METRICS_DB_PATH", &metrics_path)]);
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://example.com/codearts/project.git",
    ])
    .unwrap();
    let storage = tempfile::tempdir().unwrap();
    let database = storage.path().join("codearts.db");
    fs::copy(fixture_path("opencode-sqlite/opencode.db"), &database).unwrap();
    let update = json!({
        "hook_event_name": "SessionUpdate",
        "session_id": "test-session-123",
        "cwd": repo.canonical_path(),
        "transcript_path": database,
    });
    let config_path = repo.test_home_path().join(".git-ai/config.json");
    let original: Value = serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    let metrics = MetricsDatabase::open_at_path(Path::new(&metrics_path)).unwrap();
    for (field, pattern) in [
        ("exclude_repositories", "*"),
        ("allow_repositories", "not-this-repository/*"),
    ] {
        let mut config = original.clone();
        config[field] = json!([pattern]);
        fs::write(&config_path, config.to_string()).unwrap();
        repo.git_ai(&[
            "checkpoint",
            "codearts",
            "--hook-input",
            &update.to_string(),
        ])
        .unwrap();
        repo.git_ai(&["await", "--timeout", "60"]).unwrap();
        assert!(
            metrics
                .get_metric_history(0, None, &[MetricEventId::SessionEvent as u16])
                .unwrap()
                .is_empty(),
            "{field} must prevent session collection"
        );
    }
    // The same completed conversation is collected once the repository is allowed,
    // including a session with no editing tool call or attribution checkpoint.
    fs::write(&config_path, original.to_string()).unwrap();
    repo.git_ai(&[
        "checkpoint",
        "codearts",
        "--hook-input",
        &update.to_string(),
    ])
    .unwrap();
    repo.git_ai(&["await", "--timeout", "60"]).unwrap();
    assert_eq!(
        metrics
            .get_metric_history(0, None, &[MetricEventId::SessionEvent as u16])
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn test_codearts_e2e_streams_conversation_with_codearts_identity() {
    let (_metrics_dir, metrics_path) = isolated_metrics_db_path();
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_METRICS_DB_PATH", &metrics_path)]);
    let storage = tempfile::tempdir().unwrap();
    let database = storage.path().join("codearts.db");
    fs::copy(fixture_path("opencode-sqlite/opencode.db"), &database).unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch(
        "UPDATE part SET data = json_remove(json_set(data, '$.state.status', 'running'), '$.state.output')
             WHERE id = 'prt-sql-003';
         UPDATE message SET data = json_remove(data, '$.time.completed')
             WHERE id = 'msg-assistant-sql-001';"
    ).unwrap();
    drop(connection);
    let file_path = repo.path().join("example.txt");
    let mut file = repo.filename("example.txt");
    fs::write(&file_path, "Original line\n").unwrap();
    repo.stage_all_and_commit("Initial content").unwrap();
    file.assert_committed_lines(lines!["Original line".unattributed_human()]);

    let mut hook = hook_input("PreToolUse", "edit", json!({"filePath": "example.txt"}));
    hook["cwd"] = json!(repo.canonical_path());
    hook["session_id"] = json!("test-session-123");
    hook["tool_use_id"] = json!("call-sql-001");
    hook["model"] = json!("mimo-v2.5");
    hook["transcript_path"] = json!(database);
    repo.git_ai(&["checkpoint", "codearts", "--hook-input", &hook.to_string()])
        .unwrap();
    fs::write(&file_path, "Original line\nCodeArts generated line\n").unwrap();
    hook["hook_event_name"] = json!("PostToolUse");
    repo.git_ai(&["checkpoint", "codearts", "--hook-input", &hook.to_string()])
        .unwrap();
    let commit = repo.stage_all_and_commit("CodeArts edit").unwrap();
    file.assert_committed_lines(lines![
        "Original line".unattributed_human(),
        "CodeArts generated line".ai(),
    ]);
    let session = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .unwrap();
    assert_eq!(session.agent_id.tool, "codearts");
    assert_eq!(session.agent_id.id, "test-session-123");
    assert_eq!(session.agent_id.model, "mimo-v2.5");

    repo.sync_daemon_force();
    repo.git_ai(&["await", "--timeout", "60"]).unwrap();
    let metrics = MetricsDatabase::open_at_path(Path::new(&metrics_path)).unwrap();
    let session_id = generate_session_id("test-session-123", "codearts");
    let events: Vec<_> = metrics
        .get_metric_history(0, None, &[MetricEventId::SessionEvent as u16])
        .unwrap()
        .into_iter()
        .map(|record| {
            (
                EventAttributes::from_sparse(&record.event.attrs),
                SessionEventValues::from_sparse(&record.event.values),
            )
        })
        .filter(|(attrs, _)| attrs.session_id == Some(Some(session_id.clone())))
        .collect();
    assert_eq!(
        events.len(),
        2,
        "both user and assistant messages must be captured"
    );
    for (attrs, _) in &events {
        assert_eq!(attrs.tool, Some(Some("codearts".to_string())));
        assert_eq!(
            attrs.external_session_id,
            Some(Some("test-session-123".to_string()))
        );
        assert_eq!(
            attrs.external_parent_session_id,
            Some(Some("parent-session-456".to_string()))
        );
        assert_eq!(
            attrs.parent_session_id,
            Some(Some(generate_session_id("parent-session-456", "codearts")))
        );
    }
    let user = &events
        .iter()
        .find(|(_, event)| event.external_event_id.as_deref() == Some("msg-user-sql-001"))
        .unwrap()
        .1;
    assert_eq!(user.external_parent_event_id, None);
    assert_eq!(user.external_tool_use_id, None);
    assert_eq!(
        user.raw_json["parts"][0]["data"]["text"],
        "Please update index.ts using sqlite transcript data"
    );
    let assistant = &events
        .iter()
        .find(|(_, event)| event.external_event_id.as_deref() == Some("msg-assistant-sql-001"))
        .unwrap()
        .1;
    assert_eq!(
        assistant.external_parent_event_id.as_deref(),
        Some("msg-user-sql-001")
    );
    assert_eq!(
        assistant.external_tool_use_id.as_deref(),
        Some("call-sql-001")
    );
    assert_eq!(assistant.raw_json["message"]["data"]["modelID"], "gpt-5");
    assert_eq!(
        assistant.raw_json["parts"][0]["data"]["text"],
        "I will make the edit from sqlite."
    );
    let tool = &assistant.raw_json["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["data"]["callID"] == "call-sql-001")
        .unwrap()["data"];
    assert_eq!(tool["tool"], "edit");
    assert_eq!(tool["state"]["status"], "running");
    assert_eq!(
        tool["state"]["input"]["filePath"],
        "/Users/test/project/index.ts"
    );
    assert!(tool["state"].get("output").is_none());

    // CodeArts persists its final assistant reply after the last tool hook.
    // Refreshing that transcript must not checkpoint intervening user edits.
    fs::write(
        repo.path().join("unrelated.txt"),
        "Untracked after the tool\n",
    )
    .unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch(
        "UPDATE part SET data = json_set(data, '$.state.status', 'completed', '$.state.output', 'Edit applied'),
             time_updated = 1706459836000 WHERE id = 'prt-sql-003';
         UPDATE message SET data = json_set(data, '$.time.completed', 1706459836000),
             time_updated = 1706459836000 WHERE id = 'msg-assistant-sql-001';
         INSERT INTO message VALUES ('msg-final', 'test-session-123', 1706459837000, 1706459837000,
            '{\"role\":\"assistant\",\"parentID\":\"msg-user-sql-001\",\"time\":{\"completed\":1706459837000}}');
         INSERT INTO part VALUES ('part-final', 'msg-final', 'test-session-123', 1706459837000, 1706459837000,
            '{\"type\":\"text\",\"text\":\"Final reply after the last tool\"}');"
    ).unwrap();
    drop(connection);
    let update = json!({
        "hook_event_name": "SessionUpdate",
        "session_id": "test-session-123",
        "cwd": repo.canonical_path(),
        "transcript_path": database,
    });
    repo.git_ai(&[
        "checkpoint",
        "codearts",
        "--hook-input",
        &update.to_string(),
    ])
    .unwrap();
    repo.git_ai(&["await", "--timeout", "60"]).unwrap();
    let refreshed_events: Vec<_> = metrics
        .get_metric_history(0, None, &[MetricEventId::SessionEvent as u16])
        .unwrap()
        .into_iter()
        .filter(|record| {
            EventAttributes::from_sparse(&record.event.attrs).session_id
                == Some(Some(session_id.clone()))
        })
        .map(|record| SessionEventValues::from_sparse(&record.event.values))
        .collect();
    let updated_assistant = refreshed_events
        .iter()
        .find(|event| {
            event.external_event_id.as_deref() == Some("msg-assistant-sql-001")
                && event.raw_json["message"]["time_updated"] == 1706459836000_i64
        })
        .expect("the previously captured assistant message must be refreshed");
    let completed_tool = &updated_assistant.raw_json["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["data"]["callID"] == "call-sql-001")
        .unwrap()["data"];
    assert_eq!(completed_tool["state"]["status"], "completed");
    assert_eq!(completed_tool["state"]["output"], "Edit applied");
    let final_messages: Vec<_> = refreshed_events
        .iter()
        .filter(|event| event.external_event_id.as_deref() == Some("msg-final"))
        .collect();
    assert_eq!(
        final_messages.len(),
        1,
        "the final reply must arrive without another editing tool call"
    );
    assert_eq!(
        final_messages[0].raw_json["parts"][0]["data"]["text"],
        "Final reply after the last tool"
    );
    repo.stage_all_and_commit("Untracked edit after CodeArts")
        .unwrap();
    file.assert_committed_lines(lines![
        "Original line".unattributed_human(),
        "CodeArts generated line".ai(),
    ]);
    repo.filename("unrelated.txt")
        .assert_committed_lines(lines!["Untracked after the tool".unattributed_human()]);
}

#[test]
fn test_codearts_empty_model_is_unknown() {
    for model in [Value::Null, json!(""), json!("   ")] {
        let mut input = hook_input("PostToolUse", "edit", json!({"filePath": "main.rs"}));
        input["model"] = model;
        let events = parse_codearts(&input).unwrap();
        let ParsedHookEvent::PostFileEdit(event) = &events[0] else {
            panic!("expected a post-edit event");
        };
        assert_eq!(event.context.agent_id.model, "unknown");
    }
}

#[test]
fn test_codearts_multiedit_normalizes_and_deduplicates_paths() {
    let events = parse_codearts(&hook_input(
        "PostToolUse",
        "multiedit",
        json!({"edits": [
            {"file_path": "src/main.rs"},
            {"filePath": "src/main.rs"},
            {"path": "file:///project/src/lib.rs"},
        ]}),
    ))
    .unwrap();
    let ParsedHookEvent::PostFileEdit(event) = &events[0] else {
        panic!("expected a post-edit event");
    };
    assert_eq!(
        event.file_paths,
        vec![
            PathBuf::from("/project/src/main.rs"),
            PathBuf::from("/project/src/lib.rs"),
        ]
    );
}

#[test]
fn test_codearts_file_contents_do_not_expand_checkpoint_scope() {
    for tool in ["edit", "write"] {
        let events = parse_codearts(&hook_input(
            "PostToolUse",
            tool,
            json!({
                "filePath": "target.rs",
                "content": "*** Update File: unrelated.rs\n+example code\n",
                "oldString": "file:///other.rs",
            }),
        ))
        .unwrap();
        let ParsedHookEvent::PostFileEdit(event) = &events[0] else {
            panic!("expected a post-edit event");
        };
        assert_eq!(event.file_paths, vec![PathBuf::from("/project/target.rs")]);
    }
}

#[test]
fn test_codearts_apply_patch_tracks_added_deleted_and_moved_paths() {
    let events = parse_codearts(&hook_input(
        "PreToolUse",
        "apply_patch",
        json!({"patchText": "*** Begin Patch\n*** Add File: added.rs\n+new\n*** Delete File: deleted.rs\n*** Update File: old.rs\n*** Move to: renamed.rs\n@@\n-old\n+new\n*** End Patch"}),
    ))
    .unwrap();
    let ParsedHookEvent::PreFileEdit(event) = &events[0] else {
        panic!("expected a pre-edit event");
    };
    assert_eq!(
        event.file_paths,
        vec![
            PathBuf::from("/project/added.rs"),
            PathBuf::from("/project/deleted.rs"),
            PathBuf::from("/project/old.rs"),
            PathBuf::from("/project/renamed.rs"),
        ]
    );
}

#[test]
fn test_codearts_delete_file_tracks_target() {
    let events = parse_codearts(&hook_input(
        "PostToolUse",
        "deleteFile",
        json!({"filePath": "removed.rs"}),
    ))
    .unwrap();
    let ParsedHookEvent::PostFileEdit(event) = &events[0] else {
        panic!("expected a post-edit event");
    };
    assert_eq!(event.file_paths, vec![PathBuf::from("/project/removed.rs")]);
}

#[test]
fn test_codearts_shell_hooks_preserve_command_and_invocation_id() {
    for tool in ["bash", "shell"] {
        for command_key in ["command", "cmd"] {
            let input = json!({command_key: "  echo generated > output.txt  "});
            let pre = parse_codearts(&hook_input("PreToolUse", tool, input.clone())).unwrap();
            let ParsedHookEvent::PreBashCall(pre) = &pre[0] else {
                panic!("expected a pre-shell event for {tool}");
            };
            assert_eq!(pre.tool_use_id, "tool-call-1");
            assert_eq!(pre.command.as_deref(), Some("echo generated > output.txt"));
            assert_eq!(pre.context.agent_id.tool, "codearts");

            let post = parse_codearts(&hook_input("PostToolUse", tool, input)).unwrap();
            let ParsedHookEvent::PostBashCall(post) = &post[0] else {
                panic!("expected a post-shell event for {tool}");
            };
            assert_eq!(post.tool_use_id, pre.tool_use_id);
            assert_eq!(post.command, pre.command);
            assert!(post.stream_source.is_none());
        }
    }
}

#[test]
fn test_codearts_ignores_unknown_events_and_read_only_tools() {
    for event in ["Stop", "PostToolUseFailure", "session.updated"] {
        assert!(
            parse_codearts(&hook_input(event, "edit", json!({"filePath": "main.rs"})))
                .unwrap()
                .is_empty()
        );
    }
    for tool in ["read", "glob", "grep", "webfetch", "unknown_tool"] {
        for event in ["PreToolUse", "PostToolUse"] {
            assert!(
                parse_codearts(&hook_input(event, tool, json!({"filePath": "main.rs"})))
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

#[test]
fn test_codearts_requires_nonempty_session_cwd_and_invocation_id() {
    for field in ["session_id", "cwd", "tool_use_id"] {
        for value in [None, Some(json!("")), Some(json!("   "))] {
            let mut input = hook_input("PreToolUse", "edit", json!({"filePath": "main.rs"}));
            if let Some(value) = value {
                input[field] = value;
            } else {
                input.as_object_mut().unwrap().remove(field);
            }
            let error = parse_codearts(&input).unwrap_err();
            assert!(error.to_string().contains(field), "{field}: {error}");
        }
    }
}

#[test]
fn test_codearts_rejects_file_edits_without_resolvable_paths() {
    for event in ["PreToolUse", "PostToolUse"] {
        for input in [Value::Null, json!({}), json!({"filePath": " "})] {
            assert!(parse_codearts(&hook_input(event, "edit", input)).is_err());
        }
    }
}

fn checkpoint(repo: &TestRepo, event: &str, tool: &str, tool_id: &str, input: Value) {
    let mut hook = hook_input(event, tool, input);
    hook["cwd"] = json!(repo.canonical_path());
    hook["tool_use_id"] = json!(tool_id);
    hook["model"] = json!("deepseek-v3.2");
    repo.git_ai(&["checkpoint", "codearts", "--hook-input", &hook.to_string()])
        .unwrap();
}

#[test]
fn test_codearts_e2e_file_edit_preserves_human_and_untracked_lines() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("example.txt");
    let mut file = repo.filename("example.txt");

    fs::write(&file_path, "Original untracked line\n").unwrap();
    repo.stage_all_and_commit("Initial untracked content")
        .unwrap();
    file.assert_committed_lines(lines!["Original untracked line".unattributed_human()]);

    fs::write(&file_path, "Original untracked line\nKnown human line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "example.txt"])
        .unwrap();
    repo.stage_all_and_commit("Known human edit").unwrap();
    file.assert_committed_lines(lines![
        "Original untracked line".unattributed_human(),
        "Known human line".human(),
    ]);

    // Keep the pre-existing untracked edit apart from the AI edit: the shared
    // commit recovery intentionally extends AI attribution to adjacent lines.
    fs::write(
        &file_path,
        "Untracked before CodeArts\nOriginal untracked line\nKnown human line\n",
    )
    .unwrap();
    let input = json!({"filePath": "example.txt"});
    checkpoint(&repo, "PreToolUse", "edit", "edit-1", input.clone());
    fs::write(
        &file_path,
        "Untracked before CodeArts\nOriginal untracked line\nKnown human line\nCodeArts generated line\n",
    )
    .unwrap();
    checkpoint(&repo, "PostToolUse", "edit", "edit-1", input);
    let commit = repo.stage_all_and_commit("CodeArts file edit").unwrap();
    file.assert_committed_lines(lines![
        "Untracked before CodeArts".unattributed_human(),
        "Original untracked line".unattributed_human(),
        "Known human line".human(),
        "CodeArts generated line".ai(),
    ]);
    let sessions = &commit.authorship_log.metadata.sessions;
    assert_eq!(sessions.len(), 1);
    let session = sessions.values().next().unwrap();
    assert_eq!(session.agent_id.tool, "codearts");
    assert_eq!(session.agent_id.id, "codearts-session");
    assert_eq!(session.agent_id.model, "deepseek-v3.2");
}

#[test]
fn test_codearts_e2e_shell_edit_attributes_only_new_changes() {
    let bash_db = tempfile::tempdir().unwrap();
    let bash_db_path = bash_db.path().join("bash-checkpoints.db");
    let repo = TestRepo::new_with_daemon_env(&[(
        "GIT_AI_TEST_BASH_CHECKPOINT_DB_PATH",
        bash_db_path.to_str().unwrap(),
    )]);
    let file_path = repo.path().join("example.txt");
    let mut file = repo.filename("example.txt");

    fs::write(&file_path, "Original untracked line\n").unwrap();
    repo.stage_all_and_commit("Initial untracked content")
        .unwrap();
    file.assert_committed_lines(lines!["Original untracked line".unattributed_human()]);

    fs::write(&file_path, "Original untracked line\nKnown human line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "example.txt"])
        .unwrap();
    repo.stage_all_and_commit("Known human edit").unwrap();
    file.assert_committed_lines(lines![
        "Original untracked line".unattributed_human(),
        "Known human line".human(),
    ]);

    let input = json!({"command": "echo CodeArts shell line >> example.txt"});
    checkpoint(&repo, "PreToolUse", "bash", "bash-1", input.clone());
    fs::write(
        &file_path,
        "Original untracked line\nKnown human line\nCodeArts shell line\n",
    )
    .unwrap();
    checkpoint(&repo, "PostToolUse", "bash", "bash-1", input);
    let commit = repo.stage_all_and_commit("CodeArts shell edit").unwrap();
    file.assert_committed_lines(lines![
        "Original untracked line".unattributed_human(),
        "Known human line".human(),
        "CodeArts shell line".ai(),
    ]);
    let sessions = &commit.authorship_log.metadata.sessions;
    assert_eq!(sessions.len(), 1);
    let session = sessions.values().next().unwrap();
    assert_eq!(session.agent_id.tool, "codearts");
    assert_eq!(session.agent_id.id, "codearts-session");
    assert_eq!(session.agent_id.model, "deepseek-v3.2");
}

#[test]
fn test_codearts_e2e_multiedit_excludes_unrelated_dirty_files() {
    let repo = TestRepo::new();
    let mut first = repo.filename("first.txt");
    let mut second = repo.filename("second.txt");
    let mut outside = repo.filename("outside.txt");

    fs::write(repo.path().join("first.txt"), "First original line\n").unwrap();
    fs::write(repo.path().join("second.txt"), "Second original line\n").unwrap();
    fs::write(repo.path().join("outside.txt"), "Outside original line\n").unwrap();
    repo.stage_all_and_commit("Initial multiedit files")
        .unwrap();
    first.assert_committed_lines(lines!["First original line".unattributed_human()]);
    second.assert_committed_lines(lines!["Second original line".unattributed_human()]);
    outside.assert_committed_lines(lines!["Outside original line".unattributed_human()]);

    // These edits have no checkpoint and must remain untracked even when they
    // are committed together with CodeArts changes in other files.
    fs::write(
        repo.path().join("outside.txt"),
        "Outside original line\nUntracked outside line one\nUntracked outside line two\n",
    )
    .unwrap();
    let input = json!({"edits": [
        {"file_path": "first.txt"},
        {"filePath": "second.txt"},
        {"path": "first.txt"},
    ]});
    checkpoint(
        &repo,
        "PreToolUse",
        "multiedit",
        "multiedit-1",
        input.clone(),
    );
    fs::write(
        repo.path().join("first.txt"),
        "First original line\nFirst CodeArts addition\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("second.txt"),
        "Second original line\nSecond CodeArts addition\n",
    )
    .unwrap();
    checkpoint(&repo, "PostToolUse", "multiedit", "multiedit-1", input);
    let commit = repo.stage_all_and_commit("CodeArts multiedit").unwrap();
    first.assert_committed_lines(lines![
        "First original line".unattributed_human(),
        "First CodeArts addition".ai(),
    ]);
    second.assert_committed_lines(lines![
        "Second original line".unattributed_human(),
        "Second CodeArts addition".ai(),
    ]);
    outside.assert_committed_lines(lines![
        "Outside original line".unattributed_human(),
        "Untracked outside line one".unattributed_human(),
        "Untracked outside line two".unattributed_human(),
    ]);
    let sessions = &commit.authorship_log.metadata.sessions;
    assert_eq!(sessions.len(), 1);
    let session = sessions.values().next().unwrap();
    assert_eq!(session.agent_id.tool, "codearts");
    assert_eq!(session.agent_id.id, "codearts-session");
    assert_eq!(session.agent_id.model, "deepseek-v3.2");
}

#[test]
fn test_codearts_e2e_apply_patch_adds_and_moves_files() {
    let repo = TestRepo::new();
    let mut original = repo.filename("original.txt");
    let mut renamed = repo.filename("renamed.txt");
    let mut added = repo.filename("added.txt");
    let mut deleted = repo.filename("deleted.txt");
    let original_content = "Original first line\nOriginal second line\nOriginal third line\n";

    fs::write(repo.path().join("original.txt"), original_content).unwrap();
    fs::write(
        repo.path().join("deleted.txt"),
        "Remove this original line\n",
    )
    .unwrap();
    repo.stage_all_and_commit("Initial patch files").unwrap();
    original.assert_committed_lines(lines![
        "Original first line".unattributed_human(),
        "Original second line".unattributed_human(),
        "Original third line".unattributed_human(),
    ]);
    deleted.assert_committed_lines(lines!["Remove this original line".unattributed_human()]);

    let input = json!({"patchText": "*** Begin Patch\n*** Add File: added.txt\n+CodeArts new file\n*** Delete File: deleted.txt\n*** Update File: original.txt\n*** Move to: renamed.txt\n@@\n Original third line\n+CodeArts addition after move\n*** End Patch"});
    checkpoint(&repo, "PreToolUse", "apply_patch", "patch-1", input.clone());
    fs::rename(
        repo.path().join("original.txt"),
        repo.path().join("renamed.txt"),
    )
    .unwrap();
    fs::write(
        repo.path().join("renamed.txt"),
        format!("{original_content}CodeArts addition after move\n"),
    )
    .unwrap();
    fs::write(repo.path().join("added.txt"), "CodeArts new file\n").unwrap();
    fs::remove_file(repo.path().join("deleted.txt")).unwrap();
    checkpoint(&repo, "PostToolUse", "apply_patch", "patch-1", input);
    let commit = repo
        .stage_all_and_commit("CodeArts patch with a move")
        .unwrap();
    renamed.assert_committed_lines(lines![
        "Original first line".unattributed_human(),
        "Original second line".unattributed_human(),
        "Original third line".unattributed_human(),
        "CodeArts addition after move".ai(),
    ]);
    added.assert_committed_lines(lines!["CodeArts new file".ai()]);
    assert!(!repo.path().join("original.txt").exists());
    assert!(!repo.path().join("deleted.txt").exists());
    assert!(repo.git(&["cat-file", "-e", "HEAD:original.txt"]).is_err());
    assert!(repo.git(&["cat-file", "-e", "HEAD:deleted.txt"]).is_err());
    let sessions = &commit.authorship_log.metadata.sessions;
    assert_eq!(sessions.len(), 1);
    let session = sessions.values().next().unwrap();
    assert_eq!(session.agent_id.tool, "codearts");
    assert_eq!(session.agent_id.id, "codearts-session");
    assert_eq!(session.agent_id.model, "deepseek-v3.2");
}
