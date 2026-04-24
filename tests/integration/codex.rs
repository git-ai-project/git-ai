use crate::repos::test_file::ExpectedLineExt;
use crate::test_utils::fixture_path;
use git_ai::authorship::transcript::Message;
use git_ai::authorship::working_log::CheckpointKind;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, CodexPreset,
};
use git_ai::commands::checkpoint_agent::bash_tool;
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;
use std::thread;
use std::time::Duration;

#[test]
fn test_parse_codex_rollout_transcript() {
    let fixture = fixture_path("codex-session-simple.jsonl");
    let (transcript, model) =
        CodexPreset::transcript_and_model_from_codex_rollout_jsonl(fixture.to_str().unwrap())
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

    let result = CodexPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Codex preset should run");

    assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
    assert_eq!(result.agent_id.tool, "codex");
    assert_eq!(
        result.agent_id.id, "019c4b43-1451-7af3-be4c-5576369bf1ba",
        "Legacy thread-id should map to agent id"
    );
    assert_eq!(
        result.agent_id.model, "gpt-5-codex",
        "Model should come from transcript"
    );
    assert_eq!(
        result.repo_working_dir.as_deref(),
        Some("/Users/test/projects/git-ai")
    );
    assert!(
        result.transcript.is_some(),
        "AI checkpoint should include transcript"
    );
    assert!(
        result.edited_filepaths.is_none(),
        "Codex hooks do not provide file pathspecs"
    );
    assert!(
        result
            .agent_metadata
            .as_ref()
            .and_then(|m| m.get("transcript_path"))
            .is_some(),
        "transcript_path should be persisted for commit-time resync"
    );
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

    let result = CodexPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Codex preset should run");

    assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
    assert_eq!(result.agent_id.tool, "codex");
    assert_eq!(
        result.agent_id.id, "session-abc-123",
        "session_id should be preferred when present"
    );
    assert_eq!(
        result.agent_id.model, "gpt-5-codex",
        "Model should come from transcript"
    );
    assert_eq!(
        result.repo_working_dir.as_deref(),
        Some("/Users/test/projects/git-ai")
    );
    assert!(
        result.transcript.is_some(),
        "AI checkpoint should include transcript"
    );
}

#[test]
fn test_codex_preset_bash_pre_tool_use_skips_checkpoint_after_capturing_snapshot() {
    use crate::repos::test_repo::TestRepo;

    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();
    let fixture = fixture_path("codex-session-simple.jsonl");
    let hook_input = json!({
        "session_id": "session-bash-pre",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "bash-use-1",
        "tool_input": {
            "command": "git status --short"
        },
        "transcript_path": fixture.to_str().unwrap()
    })
    .to_string();

    match CodexPreset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    }) {
        Err(GitAiError::PresetError(message)) => {
            assert!(
                message.contains("Skipping Codex PreToolUse checkpoint"),
                "unexpected error message: {message}"
            );
        }
        other => panic!("expected Codex PreToolUse skip PresetError, got {other:?}"),
    }

    assert!(
        bash_tool::has_active_bash_inflight(&repo_root),
        "Codex PreToolUse should still capture a bash pre-snapshot"
    );

    let active_context = bash_tool::latest_inflight_bash_agent_context(&repo_root)
        .expect("active context should exist");
    assert_eq!(active_context.agent_id.tool, "codex");
    assert_eq!(active_context.session_id, "session-bash-pre");
    assert_eq!(active_context.tool_use_id, "bash-use-1");
    assert_eq!(
        active_context
            .agent_metadata
            .as_ref()
            .and_then(|m| m.get("transcript_path"))
            .map(String::as_str),
        fixture.to_str(),
        "active context should preserve transcript path for commit-time recovery"
    );
}

#[test]
fn test_codex_preset_bash_pre_tool_use_supports_camel_case_hook_event_name() {
    use crate::repos::test_repo::TestRepo;

    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();
    let fixture = fixture_path("codex-session-simple.jsonl");
    let hook_input = json!({
        "session_id": "session-bash-pre-camel",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hookEventName": "PreToolUse",
        "toolName": "Bash",
        "toolUseId": "bash-use-camel-1",
        "tool_input": {
            "command": "git status --short"
        },
        "transcript_path": fixture.to_str().unwrap()
    })
    .to_string();

    match CodexPreset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    }) {
        Err(GitAiError::PresetError(message)) => {
            assert!(
                message.contains("Skipping Codex PreToolUse checkpoint"),
                "unexpected error message: {message}"
            );
        }
        other => panic!("expected Codex PreToolUse skip PresetError, got {other:?}"),
    }

    assert!(
        bash_tool::has_active_bash_inflight(&repo_root),
        "camel-case PreToolUse should still capture a bash pre-snapshot"
    );

    let active_context = bash_tool::latest_inflight_bash_agent_context(&repo_root)
        .expect("active context should exist");
    assert_eq!(active_context.agent_id.tool, "codex");
    assert_eq!(active_context.session_id, "session-bash-pre-camel");
    assert_eq!(active_context.tool_use_id, "bash-use-camel-1");
}

#[test]
fn test_codex_preset_bash_post_tool_use_detects_changed_files() {
    use crate::repos::test_repo::TestRepo;

    let fixture = fixture_path("codex-session-simple.jsonl");
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();
    let file_path = repo_root.join("src").join("main.rs");
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let pre_hook_input = json!({
        "session_id": "session-bash-post",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "bash-use-2",
        "tool_input": {
            "command": "python - <<'PY'\nprint('before')\nPY"
        },
        "transcript_path": fixture.to_str().unwrap()
    })
    .to_string();

    match CodexPreset.run(AgentCheckpointFlags {
        hook_input: Some(pre_hook_input),
    }) {
        Err(GitAiError::PresetError(message)) => {
            assert!(
                message.contains("Skipping Codex PreToolUse checkpoint"),
                "unexpected error message: {message}"
            );
        }
        other => panic!("expected Codex PreToolUse skip PresetError, got {other:?}"),
    }

    thread::sleep(Duration::from_millis(50));
    fs::write(&file_path, "fn main() { println!(\"hello\"); }\n").unwrap();

    let post_hook_input = json!({
        "session_id": "session-bash-post",
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_use_id": "bash-use-2",
        "tool_input": {
            "command": "perl -0pi -e 's/fn main\\(\\) \\{\\}/fn main\\(\\) { println!(\"hello\"); }/' src/main.rs"
        },
        "transcript_path": fixture.to_str().unwrap()
    })
    .to_string();

    let result = CodexPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(post_hook_input),
        })
        .expect("Codex preset post-hook should run");

    assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
    assert!(
        result.transcript.is_some(),
        "PostToolUse should attach transcript content"
    );
    assert_eq!(
        result.edited_filepaths,
        Some(vec!["src/main.rs".to_string()]),
        "Bash post-hook should scope the checkpoint to changed files"
    );
}

#[test]
fn test_codex_apply_patch_pre_tool_use_records_will_edit_paths_from_command() {
    let hook_input = json!({
        "session_id": "session-apply-patch-pre",
        "cwd": "/Users/test/projects/git-ai",
        "hook_event_name": "PreToolUse",
        "model": "gpt-5.4",
        "tool_name": "apply_patch",
        "tool_use_id": "apply-patch-call-1",
        "tool_input": {
            "command": "*** Begin Patch\n*** Add File: src/new.rs\n+pub fn new() {}\n*** Update File: README.md\n@@\n-old\n+new\n*** Move to: docs/README.md\n*** Delete File: obsolete.txt\n*** End Patch"
        }
    })
    .to_string();

    let result = CodexPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Codex apply_patch pre-hook should produce a scoped human checkpoint");

    assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
    assert_eq!(result.agent_id.tool, "codex");
    assert_eq!(result.agent_id.model, "gpt-5.4");
    assert!(result.transcript.is_none());
    assert_eq!(
        result.will_edit_filepaths,
        Some(vec![
            "src/new.rs".to_string(),
            "README.md".to_string(),
            "docs/README.md".to_string(),
            "obsolete.txt".to_string(),
        ])
    );
    assert!(result.edited_filepaths.is_none());
}

#[test]
fn test_codex_apply_patch_post_tool_use_records_edited_paths_from_command() {
    let hook_input = json!({
        "session_id": "session-apply-patch-post",
        "turn_id": "turn-apply-patch-post",
        "cwd": "/Users/test/projects/git-ai",
        "hook_event_name": "PostToolUse",
        "model": "gpt-5.4",
        "tool_name": "apply_patch",
        "tool_use_id": "apply-patch-call-2",
        "tool_input": {
            "command": "*** Begin Patch\n*** Add File: src/lib.rs\n+pub mod lib {}\n*** Update File: README.md\n@@\n-old\n+new\n*** End Patch"
        },
        "tool_response": "{\"output\":\"Success. Updated the following files:\\nA src/lib.rs\\nM README.md\\n\",\"metadata\":{\"exit_code\":0}}"
    })
    .to_string();

    let result = CodexPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Codex apply_patch post-hook should produce a scoped AI checkpoint");

    assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
    assert_eq!(result.agent_id.model, "gpt-5.4");
    assert!(result.transcript.is_some());
    assert_eq!(
        result.edited_filepaths,
        Some(vec!["src/lib.rs".to_string(), "README.md".to_string()])
    );
    let metadata = result
        .agent_metadata
        .as_ref()
        .expect("scoped apply_patch checkpoints should persist turn metadata");
    assert_eq!(
        metadata.get("codex_scoped_file_edit_checkpoint"),
        Some(&"true".to_string())
    );
    assert_eq!(
        metadata.get("codex_turn_id"),
        Some(&"turn-apply-patch-post".to_string())
    );
    assert!(result.will_edit_filepaths.is_none());
}

#[test]
fn test_codex_apply_patch_post_tool_use_falls_back_to_tool_response_paths() {
    let hook_input = json!({
        "session_id": "session-apply-patch-response",
        "cwd": "/Users/test/projects/git-ai",
        "hook_event_name": "PostToolUse",
        "model": "gpt-5.4",
        "tool_name": "apply_patch",
        "tool_use_id": "apply-patch-call-3",
        "tool_input": {
            "command": "apply_patch"
        },
        "tool_response": {
            "output": "Success. Updated the following files:\nA src/from-response.rs\nM Cargo.toml\n",
            "metadata": {"exit_code": 0}
        }
    })
    .to_string();

    let result = CodexPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Codex apply_patch post-hook should parse response paths");

    assert_eq!(
        result.edited_filepaths,
        Some(vec![
            "src/from-response.rs".to_string(),
            "Cargo.toml".to_string()
        ])
    );
}

#[test]
fn test_codex_stop_hook_skips_after_scoped_apply_patch_checkpoint() {
    use crate::repos::test_repo::TestRepo;

    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();
    let readme_path = repo_root.join("README.md");
    fs::write(&readme_path, "old\n").unwrap();
    repo.stage_all_and_commit("Initial README").unwrap();

    let fixture = fixture_path("codex-session-simple.jsonl");
    let transcript_path = repo_root.join("codex-apply-patch-rollout.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();

    let session_id = "session-apply-patch-stop-skip";
    let turn_id = "turn-apply-patch-stop-skip";
    let pre_hook_input = json!({
        "session_id": session_id,
        "turn_id": turn_id,
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "model": "gpt-5.4",
        "tool_name": "apply_patch",
        "tool_use_id": "apply-patch-stop-call",
        "tool_input": {
            "command": "*** Begin Patch\n*** Update File: README.md\n@@\n-old\n+new\n*** End Patch"
        },
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    repo.git_ai(&["checkpoint", "codex", "--hook-input", &pre_hook_input])
        .expect("pre-hook checkpoint should succeed");

    fs::write(&readme_path, "new\n").unwrap();

    let post_hook_input = json!({
        "session_id": session_id,
        "turn_id": turn_id,
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "model": "gpt-5.4",
        "tool_name": "apply_patch",
        "tool_use_id": "apply-patch-stop-call",
        "tool_input": {
            "command": "*** Begin Patch\n*** Update File: README.md\n@@\n-old\n+new\n*** End Patch"
        },
        "tool_response": "{\"output\":\"Success. Updated the following files:\\nM README.md\\n\",\"metadata\":{\"exit_code\":0}}",
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    repo.git_ai(&["checkpoint", "codex", "--hook-input", &post_hook_input])
        .expect("post-hook checkpoint should succeed");

    let stop_hook_input = json!({
        "session_id": session_id,
        "turn_id": turn_id,
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "Stop",
        "model": "gpt-5.4",
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    let stop_output = repo
        .git_ai(&["checkpoint", "codex", "--hook-input", &stop_hook_input])
        .expect("stop hook should exit successfully after skipping");

    assert!(
        stop_output.contains("Skipping Codex Stop checkpoint"),
        "Stop hook should skip instead of running a broad checkpoint: {stop_output}"
    );

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("working log should be readable");
    let codex_checkpoints = checkpoints
        .iter()
        .filter(|checkpoint| {
            checkpoint.kind == CheckpointKind::AiAgent
                && checkpoint
                    .agent_id
                    .as_ref()
                    .is_some_and(|agent_id| agent_id.tool == "codex" && agent_id.id == session_id)
        })
        .collect::<Vec<_>>();

    assert_eq!(
        codex_checkpoints.len(),
        1,
        "Stop hook should not append a duplicate Codex checkpoint"
    );
    assert_eq!(
        codex_checkpoints[0]
            .agent_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("codex_turn_id"))
            .map(String::as_str),
        Some(turn_id)
    );
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
        CodexPreset::find_latest_rollout_path_for_session_in_home(session_id, temp.path())
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

    // Simulate the Codex rollout being appended/updated after checkpoint.
    fs::copy(&updated_fixture, &transcript_path).unwrap();

    let commit = repo
        .stage_all_and_commit("Apply codex refactor")
        .expect("commit should succeed");

    assert_eq!(
        commit.authorship_log.metadata.prompts.len(),
        1,
        "Expected one prompt record"
    );

    let prompt = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Prompt record should exist");

    assert_eq!(
        prompt.agent_id.tool, "codex",
        "Prompt should be attributed to codex"
    );
    assert_eq!(
        prompt.agent_id.model, "gpt-5.1-codex",
        "Commit-time resync should update the model from latest rollout"
    );
    assert!(
        prompt.messages.iter().any(|m| {
            matches!(
                m,
                Message::Assistant { text, .. } if text.contains("Implemented the refactor")
            )
        }),
        "Prompt transcript should be refreshed from latest rollout"
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
        commit.authorship_log.metadata.prompts.len(),
        1,
        "Expected one prompt record from the Codex bash context"
    );

    let prompt = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Prompt record should exist");

    assert_eq!(
        prompt.agent_id.tool, "codex",
        "Commit-time bash override should attribute the prompt to codex"
    );
    assert_eq!(
        prompt.agent_id.id, "codex-bash-session",
        "Prompt should be linked to the active Codex session"
    );

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

/// Variant of test_codex_commit_inside_bash_inflight_repeated_append_keeps_file_ai using
/// unattributed (legacy) human checkpoints. Assertions match origin/main behavior: with empty
/// attribution, all lines (including "Project README") are attributed to AI because the codex
/// session claims any unattributed content.
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
    test_codex_apply_patch_pre_tool_use_records_will_edit_paths_from_command,
    test_codex_apply_patch_post_tool_use_records_edited_paths_from_command,
    test_codex_apply_patch_post_tool_use_falls_back_to_tool_response_paths,
    test_codex_stop_hook_skips_after_scoped_apply_patch_checkpoint,
    test_find_rollout_path_for_session_in_home,
    test_codex_e2e_commit_resync_uses_latest_rollout,
    test_codex_commit_inside_bash_inflight_is_attributed_to_codex,
    test_codex_commit_inside_bash_inflight_repeated_append_keeps_file_ai,
    test_codex_file_edit_then_bash_pretooluse_does_not_steal_ai_commit_attribution,
    test_codex_file_edit_then_camel_case_bash_pretooluse_does_not_steal_ai_commit_attribution,
    test_codex_read_only_bash_post_tool_use_before_edit_does_not_steal_commit_attribution,
    test_codex_commit_inside_bash_inflight_repeated_append_keeps_file_ai_standard_human,
);
