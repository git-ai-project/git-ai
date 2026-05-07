//! Integration tests for the Augment Code (Auggie) preset.
//!
//! Exercises the full flow: real `git-ai checkpoint augment` invocation
//! through the test binary, real `TestRepo`, real attribution checks
//! against produced authorship notes.
//!
//! The hook payload schema mirrors what auggie sends per
//! https://docs.augmentcode.com/cli/hooks:
//!   - top-level: `hook_event_name`, `conversation_id`, `workspace_roots[]`
//!   - per-event: `tool_name`, `tool_input`
//!   - tool_input: `path` for `save-file`/`str-replace-editor`,
//!     `file_paths[]` for `remove-files`, `command` for `launch-process`

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;

fn parse_augment(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("augment")?.parse(hook_input, "t_test123456789a")
}

// ============================================================================
// Preset routing tests (in-process)
// ============================================================================

#[test]
fn test_augment_preset_resolves() {
    assert!(
        resolve_preset("augment").is_ok(),
        "augment preset must be registered in resolve_preset"
    );
}

#[test]
fn test_augment_routes_save_file_to_post_file_edit() {
    let hook_input = json!({
        "hook_event_name": "PostToolUse",
        "conversation_id": "conv-1",
        "workspace_roots": ["/tmp/proj"],
        "tool_name": "save-file",
        "tool_input": {"path": "/tmp/proj/main.rs", "content": "fn main() {}"},
    })
    .to_string();
    let events = parse_augment(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "augment");
            assert_eq!(e.context.agent_id.id, "conv-1");
            assert_eq!(e.context.agent_id.model, "unknown");
            assert!(
                e.transcript_source.is_none(),
                "transcript_source should be None until an Augment reader lands"
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_augment_routes_str_replace_editor_to_post_file_edit() {
    let hook_input = json!({
        "hook_event_name": "PostToolUse",
        "conversation_id": "conv-2",
        "workspace_roots": ["/tmp/proj"],
        "tool_name": "str-replace-editor",
        "tool_input": {
            "path": "/tmp/proj/lib.rs",
            "command": "str_replace",
            "old_str_1": "a",
            "new_str_1": "b",
        },
    })
    .to_string();
    let events = parse_augment(&hook_input).unwrap();
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(
                e.file_paths,
                vec![std::path::PathBuf::from("/tmp/proj/lib.rs")]
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_augment_routes_remove_files_with_array() {
    let hook_input = json!({
        "hook_event_name": "PostToolUse",
        "conversation_id": "conv-3",
        "workspace_roots": ["/tmp/proj"],
        "tool_name": "remove-files",
        "tool_input": {"file_paths": ["/tmp/proj/a.rs", "/tmp/proj/b.rs"]},
    })
    .to_string();
    let events = parse_augment(&hook_input).unwrap();
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.file_paths.len(), 2);
            assert_eq!(
                e.file_paths,
                vec![
                    std::path::PathBuf::from("/tmp/proj/a.rs"),
                    std::path::PathBuf::from("/tmp/proj/b.rs"),
                ]
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_augment_routes_launch_process_to_bash() {
    let pre = json!({
        "hook_event_name": "PreToolUse",
        "conversation_id": "conv-4",
        "workspace_roots": ["/tmp/proj"],
        "tool_name": "launch-process",
        "tool_input": {"command": "git status"},
    })
    .to_string();
    let events = parse_augment(&pre).unwrap();
    match &events[0] {
        ParsedHookEvent::PreBashCall(e) => {
            assert_eq!(e.context.agent_id.tool, "augment");
            assert_eq!(e.tool_use_id, "bash");
        }
        _ => panic!("Expected PreBashCall"),
    }

    let post = json!({
        "hook_event_name": "PostToolUse",
        "conversation_id": "conv-4",
        "workspace_roots": ["/tmp/proj"],
        "tool_name": "launch-process",
        "tool_input": {"command": "git status"},
        "tool_output": "...",
    })
    .to_string();
    let events = parse_augment(&post).unwrap();
    match &events[0] {
        ParsedHookEvent::PostBashCall(e) => {
            assert_eq!(e.context.agent_id.tool, "augment");
            assert!(e.transcript_source.is_none());
        }
        _ => panic!("Expected PostBashCall"),
    }
}

#[test]
fn test_augment_rejects_lifecycle_events() {
    for event in ["SessionStart", "SessionEnd", "Stop"] {
        let payload = json!({
            "hook_event_name": event,
            "conversation_id": "conv-rej",
            "workspace_roots": ["/tmp/proj"],
        })
        .to_string();
        let result = parse_augment(&payload);
        assert!(
            result.is_err(),
            "expected error for lifecycle event {event}, got Ok"
        );
    }
}

#[test]
fn test_augment_rejects_unsupported_tools() {
    for tool in [
        "view",
        "grep-search",
        "codebase-retrieval",
        "web-fetch",
        "web-search",
    ] {
        let payload = json!({
            "hook_event_name": "PostToolUse",
            "conversation_id": "conv-tool",
            "workspace_roots": ["/tmp/proj"],
            "tool_name": tool,
            "tool_input": {},
        })
        .to_string();
        let result = parse_augment(&payload);
        assert!(
            result.is_err(),
            "expected error for unsupported tool {tool}, got Ok"
        );
    }
}

// ============================================================================
// End-to-end tests using TestRepo
// ============================================================================

#[test]
fn test_augment_e2e_save_file_attributes_to_augment() {
    let repo = TestRepo::new();

    let file_path = repo.path().join("app.py");
    fs::write(&file_path, "def hello():\n    pass\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(
        &file_path,
        "def hello():\n    pass\ndef world():\n    pass\n",
    )
    .unwrap();

    let canonical_root = repo.canonical_path();
    let canonical_file = canonical_root.join("app.py");
    let hook_input = json!({
        "hook_event_name": "PostToolUse",
        "conversation_id": "augment-e2e-1",
        "workspace_roots": [canonical_root.to_string_lossy().to_string()],
        "tool_name": "save-file",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "def hello():\n    pass\ndef world():\n    pass\n",
        },
    })
    .to_string();

    repo.git_ai(&["checkpoint", "augment", "--hook-input", &hook_input])
        .unwrap();

    let commit = repo
        .stage_all_and_commit("Add world function")
        .expect("commit should succeed");

    let mut file = repo.filename("app.py");
    file.assert_lines_and_blame(crate::lines![
        "def hello():".human(),
        "    pass".human(),
        "def world():".ai(),
        "    pass".ai(),
    ]);

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Should have AI attestations from Augment"
    );
    assert!(
        !commit.authorship_log.metadata.sessions.is_empty(),
        "Should have a session record"
    );
    let session = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("session record should exist");
    assert_eq!(session.agent_id.tool, "augment");
    assert_eq!(session.agent_id.id, "augment-e2e-1");
    assert_eq!(
        session.agent_id.model, "unknown",
        "model defaults to 'unknown' when context not enabled"
    );
}

#[test]
fn test_augment_e2e_pre_then_post_isolates_human_lines() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("index.ts");
    fs::write(&file_path, "console.log('hello');\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let canonical_root = repo.canonical_path();
    let canonical_file = canonical_root.join("index.ts");

    let pre = json!({
        "hook_event_name": "PreToolUse",
        "conversation_id": "augment-e2e-2",
        "workspace_roots": [canonical_root.to_string_lossy().to_string()],
        "tool_name": "save-file",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "console.log('hello');\nconsole.log('from augment');\n",
        },
    })
    .to_string();
    repo.git_ai(&["checkpoint", "augment", "--hook-input", &pre])
        .unwrap();

    fs::write(
        &file_path,
        "console.log('hello');\nconsole.log('from augment');\n",
    )
    .unwrap();

    let post = json!({
        "hook_event_name": "PostToolUse",
        "conversation_id": "augment-e2e-2",
        "workspace_roots": [canonical_root.to_string_lossy().to_string()],
        "tool_name": "save-file",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "console.log('hello');\nconsole.log('from augment');\n",
        },
    })
    .to_string();
    repo.git_ai(&["checkpoint", "augment", "--hook-input", &post])
        .unwrap();

    let commit = repo
        .stage_all_and_commit("Add augment line")
        .expect("commit should succeed");

    let mut file = repo.filename("index.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('hello');".human(),
        "console.log('from augment');".ai(),
    ]);

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Should have attestations"
    );
}

#[test]
fn test_augment_e2e_str_replace_editor_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("greet.py");
    fs::write(&file_path, "def greet():\n    print('hi')\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(&file_path, "def greet():\n    print('hello world')\n").unwrap();

    let canonical_root = repo.canonical_path();
    let canonical_file = canonical_root.join("greet.py");
    let hook_input = json!({
        "hook_event_name": "PostToolUse",
        "conversation_id": "augment-e2e-3",
        "workspace_roots": [canonical_root.to_string_lossy().to_string()],
        "tool_name": "str-replace-editor",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "command": "str_replace",
            "old_str_1": "    print('hi')",
            "new_str_1": "    print('hello world')",
        },
    })
    .to_string();

    repo.git_ai(&["checkpoint", "augment", "--hook-input", &hook_input])
        .unwrap();

    let commit = repo
        .stage_all_and_commit("Replace string")
        .expect("commit should succeed");

    let mut file = repo.filename("greet.py");
    file.assert_lines_and_blame(crate::lines![
        "def greet():".human(),
        "    print('hello world')".ai(),
    ]);

    assert!(!commit.authorship_log.attestations.is_empty());
}
