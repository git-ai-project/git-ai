use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

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
