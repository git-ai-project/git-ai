//! Integration tests for the Hermes (NousResearch hermes-agent) preset.
//!
//! Exercises the full flow: real `git-ai checkpoint hermes` invocation
//! through the test binary, real `TestRepo`, real attribution checks
//! against produced authorship notes.
//!
//! The hook payload schema mirrors what hermes-agent sends per
//! <https://hermes-agent.nousresearch.com/docs/user-guide/features/hooks>:
//!   - top-level: `hook_event_name`, `tool_name`, `tool_input`,
//!     `session_id`, `cwd`, `extra`
//!   - tool_input: `path` for `write_file`/`patch`, `command` for `terminal`

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;

fn parse_hermes(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("hermes")?.parse(hook_input, "t_test123456789a")
}

#[test]
fn test_hermes_preset_resolves() {
    assert!(
        resolve_preset("hermes").is_ok(),
        "hermes preset must be registered in resolve_preset"
    );
}

#[test]
fn test_hermes_routes_write_file_to_post_file_edit() {
    let hook_input = json!({
        "hook_event_name": "post_tool_call",
        "tool_name": "write_file",
        "tool_input": {"path": "/tmp/proj/main.rs", "content": "fn main() {}"},
        "session_id": "sess_1",
        "cwd": "/tmp/proj",
        "extra": {},
    })
    .to_string();
    let events = parse_hermes(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "hermes");
            assert_eq!(e.context.agent_id.id, "sess_1");
            assert_eq!(e.context.agent_id.model, "unknown");
            assert!(
                e.transcript_source.is_none(),
                "transcript_source should be None until a Hermes reader lands"
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_hermes_routes_patch_to_post_file_edit() {
    let hook_input = json!({
        "hook_event_name": "post_tool_call",
        "tool_name": "patch",
        "tool_input": {"path": "/tmp/proj/lib.rs", "diff": "..."},
        "session_id": "sess_2",
        "cwd": "/tmp/proj",
        "extra": {},
    })
    .to_string();
    let events = parse_hermes(&hook_input).unwrap();
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
fn test_hermes_routes_terminal_to_bash() {
    let pre = json!({
        "hook_event_name": "pre_tool_call",
        "tool_name": "terminal",
        "tool_input": {"command": "ls"},
        "session_id": "sess_3",
        "cwd": "/tmp/proj",
        "extra": {},
    })
    .to_string();
    let events = parse_hermes(&pre).unwrap();
    match &events[0] {
        ParsedHookEvent::PreBashCall(e) => {
            assert_eq!(e.context.agent_id.tool, "hermes");
            assert_eq!(e.tool_use_id, "bash");
        }
        _ => panic!("Expected PreBashCall"),
    }

    let post = json!({
        "hook_event_name": "post_tool_call",
        "tool_name": "terminal",
        "tool_input": {"command": "ls"},
        "session_id": "sess_3",
        "cwd": "/tmp/proj",
        "extra": {"tool_call_id": "tc-9"},
    })
    .to_string();
    let events = parse_hermes(&post).unwrap();
    match &events[0] {
        ParsedHookEvent::PostBashCall(e) => {
            assert_eq!(e.tool_use_id, "tc-9");
            assert!(e.transcript_source.is_none());
        }
        _ => panic!("Expected PostBashCall"),
    }
}

#[test]
fn test_hermes_rejects_lifecycle_events() {
    for event in [
        "pre_llm_call",
        "post_llm_call",
        "pre_api_request",
        "post_api_request",
        "on_session_start",
        "on_session_end",
        "on_session_finalize",
        "on_session_reset",
        "subagent_stop",
        "transform_tool_result",
    ] {
        let payload = json!({
            "hook_event_name": event,
            "tool_name": null,
            "tool_input": null,
            "session_id": "sess_rej",
            "cwd": "/tmp",
            "extra": {},
        })
        .to_string();
        let result = parse_hermes(&payload);
        assert!(
            result.is_err(),
            "expected error for lifecycle event {event}, got Ok"
        );
    }
}

#[test]
fn test_hermes_rejects_unsupported_tools() {
    for tool in ["read_file", "search_files", "execute_code"] {
        let payload = json!({
            "hook_event_name": "post_tool_call",
            "tool_name": tool,
            "tool_input": {},
            "session_id": "sess_tool",
            "cwd": "/tmp",
            "extra": {},
        })
        .to_string();
        let result = parse_hermes(&payload);
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
fn test_hermes_e2e_write_file_attributes_to_hermes() {
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
        "hook_event_name": "post_tool_call",
        "tool_name": "write_file",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "def hello():\n    pass\ndef world():\n    pass\n",
        },
        "session_id": "hermes-e2e-1",
        "cwd": canonical_root.to_string_lossy().to_string(),
        "extra": {},
    })
    .to_string();

    repo.git_ai(&["checkpoint", "hermes", "--hook-input", &hook_input])
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
        "Should have AI attestations from Hermes"
    );
    let session = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("session record should exist");
    assert_eq!(session.agent_id.tool, "hermes");
    assert_eq!(session.agent_id.id, "hermes-e2e-1");
    assert_eq!(
        session.agent_id.model, "unknown",
        "model defaults to 'unknown' since shell-hook payload doesn't carry it"
    );
}

#[test]
fn test_hermes_e2e_pre_then_post_isolates_human_lines() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("index.ts");
    fs::write(&file_path, "console.log('hello');\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let canonical_root = repo.canonical_path();
    let canonical_file = canonical_root.join("index.ts");

    let pre = json!({
        "hook_event_name": "pre_tool_call",
        "tool_name": "write_file",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "console.log('hello');\nconsole.log('from hermes');\n",
        },
        "session_id": "hermes-e2e-2",
        "cwd": canonical_root.to_string_lossy().to_string(),
        "extra": {},
    })
    .to_string();
    repo.git_ai(&["checkpoint", "hermes", "--hook-input", &pre])
        .unwrap();

    fs::write(
        &file_path,
        "console.log('hello');\nconsole.log('from hermes');\n",
    )
    .unwrap();

    let post = json!({
        "hook_event_name": "post_tool_call",
        "tool_name": "write_file",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "console.log('hello');\nconsole.log('from hermes');\n",
        },
        "session_id": "hermes-e2e-2",
        "cwd": canonical_root.to_string_lossy().to_string(),
        "extra": {},
    })
    .to_string();
    repo.git_ai(&["checkpoint", "hermes", "--hook-input", &post])
        .unwrap();

    let commit = repo
        .stage_all_and_commit("Add hermes line")
        .expect("commit should succeed");

    let mut file = repo.filename("index.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('hello');".human(),
        "console.log('from hermes');".ai(),
    ]);

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Should have attestations"
    );
}

#[test]
fn test_hermes_e2e_patch_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("greet.py");
    fs::write(&file_path, "def greet():\n    print('hi')\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(&file_path, "def greet():\n    print('hello world')\n").unwrap();

    let canonical_root = repo.canonical_path();
    let canonical_file = canonical_root.join("greet.py");
    let hook_input = json!({
        "hook_event_name": "post_tool_call",
        "tool_name": "patch",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "diff": "@@ -2 +2 @@\n-    print('hi')\n+    print('hello world')\n",
        },
        "session_id": "hermes-e2e-3",
        "cwd": canonical_root.to_string_lossy().to_string(),
        "extra": {},
    })
    .to_string();

    repo.git_ai(&["checkpoint", "hermes", "--hook-input", &hook_input])
        .unwrap();

    let commit = repo
        .stage_all_and_commit("Patch greet")
        .expect("commit should succeed");

    let mut file = repo.filename("greet.py");
    file.assert_lines_and_blame(crate::lines![
        "def greet():".human(),
        "    print('hello world')".ai(),
    ]);

    assert!(!commit.authorship_log.attestations.is_empty());
}
