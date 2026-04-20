use crate::repos::test_file::ExpectedLineExt;
use crate::test_utils::fixture_path;
use git_ai::authorship::transcript::Message;
use git_ai::commands::checkpoint_agent::presets::{
    BashPreHookStrategy, ParsedHookEvent, resolve_preset,
};
use git_ai::commands::checkpoint_agent::transcript_readers;
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;

fn parse_codex(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("codex")?.parse(hook_input, "t_test")
}

#[test]
fn test_parse_codex_rollout_transcript() {
    let fixture = fixture_path("codex-session-simple.jsonl");
    let (transcript, model) = transcript_readers::read_codex_jsonl(fixture.as_path())
        .expect("Failed to parse Codex rollout");

    assert!(
        !transcript.messages().is_empty(),
        "Transcript should contain messages"
    );
    assert_eq!(
        model.as_deref(),
        Some("gpt-5-codex"),
        "Model should come from turn_context.model"
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

    assert!(has_user, "Should parse user messages");
    assert!(has_assistant, "Should parse assistant messages");
    assert!(has_tool_use, "Should parse function calls as tool uses");
}

#[test]
fn test_codex_preset_legacy_hook_input() {
    let fixture = fixture_path("codex-session-simple.jsonl");
    let hook_input = json!({
        "type": "agent-turn-complete",
        "thread-id": "019c4b43-1451-7af3-be4c-5576369bf1ba",
        "turn-id": "turn-1",
        "cwd": "/Users/test/projects/git-ai",
        "input-messages": ["Refactor src/main.rs"],
        "last-assistant-message": "Done.",
        "transcript_path": fixture.to_str().unwrap()
    })
    .to_string();

    let events = parse_codex(&hook_input).expect("Codex preset should run");

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "codex");
            assert_eq!(
                e.context.agent_id.id, "019c4b43-1451-7af3-be4c-5576369bf1ba",
                "Legacy thread-id should map to agent id"
            );
            assert_eq!(
                e.context.cwd.to_string_lossy(),
                "/Users/test/projects/git-ai"
            );
            assert!(e.transcript_source.is_some());
            assert!(
                e.context.metadata.contains_key("transcript_path"),
                "transcript_path should be persisted for commit-time resync"
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_codex_preset_structured_hook_input() {
    let fixture = fixture_path("codex-session-simple.jsonl");
    let hook_input = json!({
        "session_id": "session-abc-123",
        "cwd": "/Users/test/projects/git-ai",
        "triggered_at": "2026-02-11T05:53:33Z",
        "hook_event": {
            "event_type": "after_agent",
            "thread_id": "thread-xyz-999",
            "turn_id": "turn-2",
            "input_messages": ["Refactor src/main.rs"],
            "last_assistant_message": "Done."
        },
        "transcript_path": fixture.to_str().unwrap()
    })
    .to_string();

    let events = parse_codex(&hook_input).expect("Codex preset should run");

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "codex");
            assert_eq!(
                e.context.session_id, "session-abc-123",
                "session_id should be preferred when present"
            );
            assert_eq!(
                e.context.cwd.to_string_lossy(),
                "/Users/test/projects/git-ai"
            );
            assert!(e.transcript_source.is_some());
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_codex_preset_bash_pre_tool_use_skips_checkpoint_after_capturing_snapshot() {
    let fixture = fixture_path("codex-session-simple.jsonl");
    let hook_input = json!({
        "session_id": "session-bash-pre",
        "cwd": "/tmp/test-project",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "bash-use-1",
        "tool_input": {
            "command": "git status --short"
        },
        "transcript_path": fixture.to_str().unwrap()
    })
    .to_string();

    // In the new parse API, bash PreToolUse returns PreBashCall with SnapshotOnly strategy
    // instead of returning an error. The caller handles the side effects.
    let events = parse_codex(&hook_input).expect("should succeed with PreBashCall");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PreBashCall(e) => {
            assert_eq!(e.strategy, BashPreHookStrategy::SnapshotOnly);
            assert_eq!(e.context.agent_id.tool, "codex");
            assert_eq!(e.context.session_id, "session-bash-pre");
            assert_eq!(e.tool_use_id, "bash-use-1");
            assert!(
                e.context.metadata.contains_key("transcript_path"),
                "metadata should preserve transcript path for commit-time recovery"
            );
        }
        _ => panic!("Expected PreBashCall for bash PreToolUse"),
    }
}

#[test]
fn test_codex_preset_bash_pre_tool_use_supports_camel_case_hook_event_name() {
    let fixture = fixture_path("codex-session-simple.jsonl");
    let hook_input = json!({
        "session_id": "session-bash-pre-camel",
        "cwd": "/tmp/test-project",
        "hookEventName": "PreToolUse",
        "toolName": "Bash",
        "toolUseId": "bash-use-camel-1",
        "tool_input": {
            "command": "git status --short"
        },
        "transcript_path": fixture.to_str().unwrap()
    })
    .to_string();

    // Camel-case fields should work the same as snake_case
    let events = parse_codex(&hook_input).expect("should succeed with PreBashCall");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PreBashCall(e) => {
            assert_eq!(e.strategy, BashPreHookStrategy::SnapshotOnly);
            assert_eq!(e.context.agent_id.tool, "codex");
            assert_eq!(e.context.session_id, "session-bash-pre-camel");
            assert_eq!(e.tool_use_id, "bash-use-camel-1");
        }
        _ => panic!("Expected PreBashCall for camel-case PreToolUse"),
    }
}

#[test]
fn test_codex_preset_bash_post_tool_use_detects_changed_files() {
    let fixture = fixture_path("codex-session-simple.jsonl");
    let post_hook_input = json!({
        "session_id": "session-bash-post",
        "cwd": "/tmp/test-project",
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_use_id": "bash-use-2",
        "tool_input": {
            "command": "perl -0pi -e 's/fn main\\(\\) \\{\\}/fn main\\(\\) { println!(\"hello\"); }/' src/main.rs"
        },
        "transcript_path": fixture.to_str().unwrap()
    })
    .to_string();

    let events = parse_codex(&post_hook_input).expect("Codex preset post-hook should run");

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostBashCall(e) => {
            assert!(e.transcript_source.is_some());
            assert_eq!(e.context.agent_id.tool, "codex");
            assert_eq!(e.context.session_id, "session-bash-post");
            assert_eq!(e.tool_use_id, "bash-use-2");
        }
        _ => panic!("Expected PostBashCall"),
    }
}

#[test]
fn test_find_rollout_path_for_session_in_home() {
    let fixture = fixture_path("codex-session-simple.jsonl");
    let temp = tempfile::tempdir().unwrap();

    let session_id = "019c4b43-1451-7af3-be4c-5576369bf1ba";
    let rollout_dir = temp.path().join("sessions/2026/02/11");
    fs::create_dir_all(&rollout_dir).unwrap();
    let rollout_path = rollout_dir.join(format!("rollout-2026-02-11T05-53-33-{session_id}.jsonl"));
    fs::copy(&fixture, &rollout_path).unwrap();

    let resolved =
        transcript_readers::find_codex_rollout_path_for_session_in_home(session_id, temp.path())
            .expect("search should succeed")
            .expect("rollout should be found");

    assert_eq!(resolved, rollout_path);
}

#[test]
fn test_codex_e2e_commit_resync_uses_latest_rollout() {
    use crate::repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });

    let repo_root = repo.canonical_path();
    let src_dir = repo_root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("main.rs");
    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let simple_fixture = fixture_path("codex-session-simple.jsonl");
    let updated_fixture = fixture_path("codex-session-updated.jsonl");
    let transcript_path = repo_root.join("codex-rollout.jsonl");
    let thread_id = format!("codex-e2e-{}", repo_root.to_string_lossy());
    fs::copy(&simple_fixture, &transcript_path).unwrap();

    let hook_input = json!({
        "type": "agent-turn-complete",
        "thread-id": thread_id,
        "turn-id": "turn-1",
        "cwd": repo_root.to_string_lossy().to_string(),
        "input-messages": ["Refactor src/main.rs"],
        "last-assistant-message": "Done.",
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    fs::write(
        &file_path,
        "fn greet() { println!(\"hello\"); }\nfn main() { greet(); }\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "codex", "--hook-input", &hook_input])
        .expect("checkpoint should succeed");

    fs::copy(&updated_fixture, &transcript_path).unwrap();

    let commit = repo
        .stage_all_and_commit("Apply codex refactor")
        .expect("commit should succeed");

    assert_eq!(
        commit.authorship_log.metadata.sessions.len(),
        1,
        "Expected one session record"
    );

    let session = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("Session record should exist");

    assert_eq!(session.agent_id.tool, "codex");
    assert_eq!(
        session.agent_id.model, "gpt-5.1-codex",
        "Commit-time resync should update the model from latest rollout"
    );
    assert!(
        session.messages.iter().any(|m| {
            matches!(
                m,
                Message::Assistant { text, .. } if text.contains("Implemented the refactor")
            )
        }),
        "Session transcript should be refreshed from latest rollout"
    );
}

#[test]
fn test_codex_commit_inside_bash_inflight_is_attributed_to_codex() {
    use crate::repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });

    let repo_root = repo.canonical_path();
    let src_dir = repo_root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("main.rs");
    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let simple_fixture = fixture_path("codex-session-simple.jsonl");
    let transcript_path = repo_root.join("codex-bash-rollout.jsonl");
    fs::copy(&simple_fixture, &transcript_path).unwrap();

    let pre_hook_input = json!({
        "session_id": "codex-bash-session",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "bash-use-commit",
        "tool_input": {
            "command": "python - <<'PY'\nprint('commit from codex bash')\nPY"
        },
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    repo.git_ai(&["checkpoint", "codex", "--hook-input", &pre_hook_input])
        .expect("pre-hook checkpoint should succeed");

    fs::write(
        &file_path,
        "fn greet() { println!(\"hello\"); }\nfn main() { greet(); }\n",
    )
    .unwrap();

    let commit = repo
        .stage_all_and_commit("Apply codex bash refactor")
        .expect("commit should succeed");

    assert_eq!(
        commit.authorship_log.metadata.sessions.len(),
        1,
        "Expected one session record from the Codex bash context"
    );

    let session = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("Session record should exist");

    assert_eq!(session.agent_id.tool, "codex");
    assert_eq!(session.agent_id.id, "codex-bash-session");

    let mut tracked_file = repo.filename("src/main.rs");
    tracked_file.assert_lines_and_blame(crate::lines![
        "fn greet() { println!(\"hello\"); }".ai(),
        "fn main() { greet(); }".ai(),
    ]);
}

#[test]
fn test_codex_commit_inside_bash_inflight_repeated_append_keeps_file_ai() {
    use crate::repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["Project README"]);
    repo.stage_all_and_commit("Initial README")
        .expect("initial README commit should succeed");

    let repo_root = repo.canonical_path();
    let simple_fixture = fixture_path("codex-session-simple.jsonl");
    let transcript_path = repo_root.join("codex-bash-append-rollout.jsonl");
    fs::copy(&simple_fixture, &transcript_path).unwrap();

    let pre_hook_input = json!({
        "session_id": "codex-bash-append-session",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "bash-use-append-commit",
        "tool_input": {
            "command": "git add README.md && git commit -m 'Codex append proof'"
        },
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    repo.git_ai(&["checkpoint", "codex", "--hook-input", &pre_hook_input])
        .expect("pre-hook checkpoint should succeed");

    readme.set_contents(crate::lines!["Project README", "Updated by Codex".ai()]);
    repo.stage_all_and_commit("Codex append proof")
        .expect("Codex append commit should succeed");

    readme.assert_lines_and_blame(crate::lines![
        "Project README".human(),
        "Updated by Codex".ai(),
    ]);

    let second_pre_hook_input = json!({
        "session_id": "codex-bash-append-session-2",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "bash-use-append-commit-2",
        "tool_input": {
            "command": "git add README.md && git commit -m 'Codex append proof 2'"
        },
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    repo.git_ai(&[
        "checkpoint",
        "codex",
        "--hook-input",
        &second_pre_hook_input,
    ])
    .expect("second pre-hook checkpoint should succeed");

    readme.set_contents(crate::lines![
        "Project README",
        "Updated by Codex".ai(),
        "Updated again by Codex".ai(),
    ]);
    repo.stage_all_and_commit("Codex append proof 2")
        .expect("second Codex append commit should succeed");

    readme.assert_lines_and_blame(crate::lines![
        "Project README".human(),
        "Updated by Codex".ai(),
        "Updated again by Codex".ai(),
    ]);
}

#[test]
fn test_codex_file_edit_then_bash_pretooluse_does_not_steal_ai_commit_attribution() {
    use crate::repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["Project README"]);
    repo.stage_all_and_commit("Initial README").unwrap();

    let repo_root = repo.canonical_path();
    let simple_fixture = fixture_path("codex-session-simple.jsonl");
    let transcript_path = repo_root.join("codex-bash-status-rollout.jsonl");
    fs::copy(&simple_fixture, &transcript_path).unwrap();

    fs::write(
        repo_root.join("README.md"),
        "Project README\nUpdated by live Codex proof\n",
    )
    .unwrap();

    let pre_hook_input = json!({
        "session_id": "codex-status-session",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "bash-use-status",
        "tool_input": {
            "command": "git status --short -- README.md"
        },
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    repo.git_ai(&["checkpoint", "codex", "--hook-input", &pre_hook_input])
        .expect("pre-hook checkpoint should succeed");

    repo.stage_all_and_commit("Codex status commit")
        .expect("Codex status commit should succeed");

    readme.assert_lines_and_blame(crate::lines![
        "Project README".ai(),
        "Updated by live Codex proof".ai(),
    ]);
}

#[test]
fn test_codex_file_edit_then_camel_case_bash_pretooluse_does_not_steal_ai_commit_attribution() {
    use crate::repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["Project README"]);
    repo.stage_all_and_commit("Initial README").unwrap();

    let repo_root = repo.canonical_path();
    let simple_fixture = fixture_path("codex-session-simple.jsonl");
    let transcript_path = repo_root.join("codex-bash-status-rollout-camel.jsonl");
    fs::copy(&simple_fixture, &transcript_path).unwrap();

    fs::write(
        repo_root.join("README.md"),
        "Project README\nUpdated by live Codex proof camel\n",
    )
    .unwrap();

    let pre_hook_input = json!({
        "session_id": "codex-status-session-camel",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hookEventName": "PreToolUse",
        "toolName": "Bash",
        "toolUseId": "bash-use-status-camel",
        "tool_input": {
            "command": "git status --short -- README.md"
        },
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    repo.git_ai(&["checkpoint", "codex", "--hook-input", &pre_hook_input])
        .expect("pre-hook checkpoint should succeed");

    repo.stage_all_and_commit("Codex status camel commit")
        .expect("Codex status camel commit should succeed");

    readme.assert_lines_and_blame(crate::lines![
        "Project README".ai(),
        "Updated by live Codex proof camel".ai(),
    ]);
}

#[test]
fn test_codex_read_only_bash_post_tool_use_before_edit_does_not_steal_commit_attribution() {
    use crate::repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["Project README"]);
    repo.stage_all_and_commit("Initial README").unwrap();

    let repo_root = repo.canonical_path();
    let simple_fixture = fixture_path("codex-session-simple.jsonl");
    let transcript_path = repo_root.join("codex-live-readonly-rollout.jsonl");
    fs::copy(&simple_fixture, &transcript_path).unwrap();

    let which_git_pre = json!({
        "session_id": "codex-live-readonly-session",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "which-git",
        "tool_input": { "command": "which git" },
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();
    repo.git_ai(&["checkpoint", "codex", "--hook-input", &which_git_pre])
        .expect("read-only pre-hook should succeed");

    let which_git_post = json!({
        "session_id": "codex-live-readonly-session",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_use_id": "which-git",
        "tool_input": { "command": "which git" },
        "tool_response": "/usr/bin/git\n",
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();
    repo.git_ai(&["checkpoint", "codex", "--hook-input", &which_git_post])
        .expect("read-only post-hook should succeed");

    fs::write(
        repo_root.join("README.md"),
        "Project README\nUpdated after read-only bash\n",
    )
    .unwrap();

    let commit_pre = json!({
        "session_id": "codex-live-readonly-session",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "commit-bash",
        "tool_input": {
            "command": "git add README.md && git commit -m \"Codex readonly bash commit\""
        },
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();
    repo.git_ai(&["checkpoint", "codex", "--hook-input", &commit_pre])
        .expect("commit pre-hook should succeed");

    repo.stage_all_and_commit("Codex readonly bash commit")
        .expect("commit should succeed");

    readme.assert_lines_and_blame(crate::lines![
        "Project README".ai(),
        "Updated after read-only bash".ai(),
    ]);
}

#[test]
fn test_codex_commit_inside_bash_inflight_repeated_append_keeps_file_ai_standard_human() {
    use crate::repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["Project README".unattributed_human()]);
    repo.stage_all_and_commit("Initial README")
        .expect("initial README commit should succeed");

    let repo_root = repo.canonical_path();
    let simple_fixture = fixture_path("codex-session-simple.jsonl");
    let transcript_path = repo_root.join("codex-bash-append-rollout-standard-human.jsonl");
    fs::copy(&simple_fixture, &transcript_path).unwrap();

    let pre_hook_input = json!({
        "session_id": "codex-bash-append-session-sh",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "bash-use-append-commit-sh",
        "tool_input": {
            "command": "git add README.md && git commit -m 'Codex append proof'"
        },
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    repo.git_ai(&["checkpoint", "codex", "--hook-input", &pre_hook_input])
        .expect("pre-hook checkpoint should succeed");

    readme.set_contents(crate::lines![
        "Project README".unattributed_human(),
        "Updated by Codex".unattributed_human()
    ]);
    repo.stage_all_and_commit("Codex append proof")
        .expect("Codex append commit should succeed");

    readme.assert_lines_and_blame(crate::lines![
        "Project README".ai(),
        "Updated by Codex".ai(),
    ]);

    let second_pre_hook_input = json!({
        "session_id": "codex-bash-append-session-2-sh",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "bash-use-append-commit-2-sh",
        "tool_input": {
            "command": "git add README.md && git commit -m 'Codex append proof 2'"
        },
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    repo.git_ai(&[
        "checkpoint",
        "codex",
        "--hook-input",
        &second_pre_hook_input,
    ])
    .expect("second pre-hook checkpoint should succeed");

    readme.set_contents(crate::lines![
        "Project README".unattributed_human(),
        "Updated by Codex".unattributed_human(),
        "Updated again by Codex".unattributed_human(),
    ]);
    repo.stage_all_and_commit("Codex append proof 2")
        .expect("second Codex append commit should succeed");

    readme.assert_lines_and_blame(crate::lines![
        "Project README".ai(),
        "Updated by Codex".ai(),
        "Updated again by Codex".ai(),
    ]);
}

crate::reuse_tests_in_worktree!(
    test_parse_codex_rollout_transcript,
    test_codex_preset_legacy_hook_input,
    test_codex_preset_structured_hook_input,
    test_codex_preset_bash_pre_tool_use_skips_checkpoint_after_capturing_snapshot,
    test_codex_preset_bash_pre_tool_use_supports_camel_case_hook_event_name,
    test_codex_preset_bash_post_tool_use_detects_changed_files,
    test_find_rollout_path_for_session_in_home,
    test_codex_e2e_commit_resync_uses_latest_rollout,
    test_codex_commit_inside_bash_inflight_is_attributed_to_codex,
    test_codex_commit_inside_bash_inflight_repeated_append_keeps_file_ai,
    test_codex_file_edit_then_bash_pretooluse_does_not_steal_ai_commit_attribution,
    test_codex_file_edit_then_camel_case_bash_pretooluse_does_not_steal_ai_commit_attribution,
    test_codex_read_only_bash_post_tool_use_before_edit_does_not_steal_commit_attribution,
    test_codex_commit_inside_bash_inflight_repeated_append_keeps_file_ai_standard_human,
);
