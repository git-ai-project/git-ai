#[macro_use]
mod repos;
mod test_utils;

use git_ai::authorship::transcript::Message;
use git_ai::authorship::working_log::CheckpointKind;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, CodexPreset,
};
use serde_json::json;
use std::fs;
use test_utils::fixture_path;

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
    use repos::test_repo::TestRepo;

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
    fs::copy(&simple_fixture, &transcript_path).unwrap();

    let hook_input = json!({
        "type": "agent-turn-complete",
        "thread-id": "019c4b43-1451-7af3-be4c-5576369bf1ba",
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
