//! Integration tests for the CodeBuddy CN preset and installer.
//!
//! These exercise the end-to-end flow: real `git-ai checkpoint codebuddy`
//! invocation via the test binary, real TestRepo, real attribution checks.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;

fn parse_codebuddy(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("codebuddy")?.parse(hook_input, "t_test")
}

// ============================================================================
// Preset routing tests (in-process; no TestRepo required)
// ============================================================================

#[test]
fn test_codebuddy_preset_resolves() {
    assert!(resolve_preset("codebuddy").is_ok());
}

#[test]
fn test_codebuddy_preset_post_file_edit_routes_correctly() {
    let hook_input = json!({
        "session_id": "sess-1",
        "transcript_path": "/Users/u/Library/Application Support/CodeBuddyExtension/Data/x/CodeBuddyIDE/y/history/z/sess-1/index.json",
        "cwd": "/",
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": {"filePath": "/tmp/proj/file.rs"},
        "model": "glm-5.0-turbo",
        "client": "CodeBuddyIDE"
    })
    .to_string();
    let events = parse_codebuddy(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "codebuddy");
            assert_eq!(e.context.agent_id.model, "glm-5.0-turbo");
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_codebuddy_preset_pre_file_edit_routes_to_human_checkpoint() {
    let hook_input = json!({
        "session_id": "sess-pre",
        "transcript_path": "/Users/u/Library/Application Support/CodeBuddyExtension/Data/x/CodeBuddyIDE/y/history/z/sess-pre/index.json",
        "cwd": "/",
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "tool_input": {"filePath": "/tmp/proj/file.rs"},
        "model": "glm-5.0-turbo",
        "client": "CodeBuddyIDE"
    })
    .to_string();
    let events = parse_codebuddy(&hook_input).unwrap();
    match &events[0] {
        ParsedHookEvent::PreFileEdit(_) => (),
        _ => panic!("Expected PreFileEdit"),
    }
}

// ============================================================================
// End-to-end tests using TestRepo
// ============================================================================

#[test]
fn test_codebuddy_e2e_post_file_edit_attributes_to_codebuddy() {
    let repo = TestRepo::new();

    let file_path = repo.path().join("app.py");
    fs::write(&file_path, "def hello():\n    pass\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(
        &file_path,
        "def hello():\n    pass\ndef world():\n    pass\n",
    )
    .unwrap();

    // CodeBuddy CN sends cwd:"/" — repo discovery comes from the absolute filePath.
    let hook_input = json!({
        "session_id": "cb-e2e-1",
        "transcript_path": "/Users/u/Library/Application Support/CodeBuddyExtension/Data/x/CodeBuddyIDE/y/history/z/cb-e2e-1/index.json",
        "cwd": "/",
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": {"filePath": file_path.to_string_lossy().to_string(), "new_str": "def hello():\n    pass\ndef world():\n    pass\n"},
        "tool_response": {"type": "replace_in_file_result", "path": file_path.to_string_lossy().to_string()},
        "generation_id": "gen-e2e-1",
        "model": "glm-5.0-turbo",
        "client": "CodeBuddyIDE",
        "version": "4.7.0"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "codebuddy", "--hook-input", &hook_input])
        .unwrap();

    let commit = repo.stage_all_and_commit("CodeBuddy edit").unwrap();

    let mut file = repo.filename("app.py");
    file.assert_lines_and_blame(crate::lines![
        "def hello():".human(),
        "    pass".human(),
        "def world():".ai(),
        "    pass".ai(),
    ]);

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Should have AI attestations from CodeBuddy"
    );
    assert!(!commit.authorship_log.metadata.sessions.is_empty());

    let session_record = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("Should have a session record");

    assert_eq!(session_record.agent_id.tool, "codebuddy");
    assert_eq!(session_record.agent_id.id, "cb-e2e-1");
    assert_eq!(session_record.agent_id.model, "glm-5.0-turbo");
}

#[test]
fn test_codebuddy_e2e_pre_file_edit_treats_subsequent_changes_as_human() {
    let repo = TestRepo::new();

    let file_path = repo.path().join("script.py");
    fs::write(&file_path, "x = 1\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // PreToolUse fires before the AI writes anything — captures human baseline.
    let hook_input = json!({
        "session_id": "cb-pre-1",
        "transcript_path": "/Users/u/Library/Application Support/CodeBuddyExtension/Data/x/CodeBuddyIDE/y/history/z/cb-pre-1/index.json",
        "cwd": "/",
        "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
        "tool_input": {"filePath": file_path.to_string_lossy().to_string()},
        "model": "glm-5.0-turbo",
        "client": "CodeBuddyIDE",
        "version": "4.7.0"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "codebuddy", "--hook-input", &hook_input])
        .unwrap();

    // User then makes manual edits without a PostToolUse — those must remain human.
    fs::write(&file_path, "x = 1\ny = 2\n").unwrap();

    let commit = repo.stage_all_and_commit("Manual follow-up").unwrap();

    let mut file = repo.filename("script.py");
    file.assert_lines_and_blame(crate::lines!["x = 1".human(), "y = 2".human(),]);

    assert_eq!(
        commit.authorship_log.attestations.len(),
        0,
        "PreToolUse-only should produce no AI attestations"
    );
}

#[test]
fn test_codebuddy_e2e_filepath_resolves_repo_when_cwd_is_root() {
    // Specifically exercises the cwd:"/" workaround — repo discovery uses
    // file_paths[0], not cwd.
    let repo = TestRepo::new();

    let file_path = repo.path().join("nested").join("module.py");
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "# initial\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(&file_path, "# initial\n# added by codebuddy\n").unwrap();

    let hook_input = json!({
        "session_id": "cb-cwd-root",
        "transcript_path": "/Users/u/Library/Application Support/CodeBuddyExtension/Data/x/CodeBuddyIDE/y/history/z/cb-cwd-root/index.json",
        "cwd": "/",
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": {"filePath": file_path.to_string_lossy().to_string()},
        "model": "glm-5.0-turbo",
        "client": "CodeBuddyIDE",
        "version": "4.7.0"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "codebuddy", "--hook-input", &hook_input])
        .unwrap();

    let commit = repo.stage_all_and_commit("CodeBuddy nested edit").unwrap();

    let mut file = repo.filename("nested/module.py");
    file.assert_lines_and_blame(crate::lines![
        "# initial".human(),
        "# added by codebuddy".ai(),
    ]);

    let session_record = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("Should have a session record");
    assert_eq!(session_record.agent_id.tool, "codebuddy");
}
