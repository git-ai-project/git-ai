use crate::repos::test_repo::TestRepo;
use git_ai::authorship::working_log::CheckpointKind;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

fn parse_junie(hook_input: &str) -> Result<Vec<ParsedHookEvent>, git_ai::error::GitAiError> {
    resolve_preset("junie")?.parse(hook_input, "t_test")
}

#[test]
fn test_junie_session_start_returns_untracked_edit_event() {
    let hook_input = json!({
        "hook_event_name": "SessionStart",
        "source": "startup",
        "cwd": "/Users/test/project"
    })
    .to_string();

    let events = parse_junie(&hook_input).expect("Junie SessionStart should parse");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::UntrackedEdit(edit) => {
            assert_eq!(edit.trace_id, "t_test");
            assert_eq!(edit.cwd, PathBuf::from("/Users/test/project"));
        }
        _ => panic!("Expected UntrackedEdit for Junie SessionStart"),
    }
}

#[test]
fn test_junie_rejects_unsupported_source() {
    let hook_input = json!({
        "hook_event_name": "SessionStart",
        "source": "clear"
    })
    .to_string();

    let result = parse_junie(&hook_input);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Unsupported Junie source")
    );
}

#[test]
fn test_junie_session_start_checkpoints_dirty_files_as_human_baseline() {
    let repo = TestRepo::new();
    fs::write(repo.path().join("existing.txt"), "base\n").expect("write base file");
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    let src_dir = repo.path().join("src");
    fs::create_dir_all(&src_dir).expect("create src directory");
    fs::write(src_dir.join("main.rs"), "fn main() {}\n").expect("write dirty file");

    let hook_input = json!({
        "hook_event_name": "SessionStart",
        "source": "startup"
    })
    .to_string();

    repo.git_ai_with_stdin(
        &["checkpoint", "junie", "--hook-input", "stdin"],
        hook_input.as_bytes(),
    )
    .expect("Junie checkpoint should succeed");

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    let latest = checkpoints.last().expect("Junie checkpoint should exist");
    assert_eq!(latest.kind, CheckpointKind::Human);
    assert!(
        latest
            .entries
            .iter()
            .any(|entry| entry.file == "src/main.rs"),
        "Junie SessionStart should baseline current dirty files"
    );
}
