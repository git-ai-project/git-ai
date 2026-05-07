//! Integration tests for the Kiro (kiro-cli, AWS Kiro IDE) preset.
//!
//! Exercises the full flow: real `git-ai checkpoint kiro` invocation
//! through the test binary, real `TestRepo`, real attribution checks
//! against produced authorship notes.
//!
//! The hook payload schema mirrors what kiro-cli sends per
//! <https://kiro.dev/docs/cli/hooks/>:
//!   - top-level: `hook_event_name` (camelCase), `cwd`, `session_id`
//!   - per-event: `tool_name` (snake_case), `tool_input`
//!   - tool_input: `path` for `fs_write`, `command` for `execute_bash`

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;

fn parse_kiro(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("kiro")?.parse(hook_input, "t_test123456789a")
}

#[test]
fn test_kiro_preset_resolves() {
    assert!(
        resolve_preset("kiro").is_ok(),
        "kiro preset must be registered in resolve_preset"
    );
}

#[test]
fn test_kiro_routes_fs_write_to_post_file_edit() {
    let hook_input = json!({
        "hook_event_name": "postToolUse",
        "cwd": "/tmp/proj",
        "session_id": "sess-1",
        "tool_name": "fs_write",
        "tool_input": {"path": "/tmp/proj/main.rs", "content": "fn main() {}"},
    })
    .to_string();
    let events = parse_kiro(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "kiro");
            assert_eq!(e.context.agent_id.id, "sess-1");
            assert_eq!(e.context.agent_id.model, "unknown");
            assert!(
                e.transcript_source.is_none(),
                "transcript_source should be None until a Kiro reader lands"
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_kiro_routes_write_alias_to_post_file_edit() {
    let hook_input = json!({
        "hook_event_name": "postToolUse",
        "cwd": "/tmp/proj",
        "session_id": "sess-2",
        "tool_name": "write",
        "tool_input": {"path": "/tmp/proj/lib.rs", "content": "..."},
    })
    .to_string();
    let events = parse_kiro(&hook_input).unwrap();
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
fn test_kiro_routes_execute_bash_to_bash() {
    let pre = json!({
        "hook_event_name": "preToolUse",
        "cwd": "/tmp/proj",
        "session_id": "sess-3",
        "tool_name": "execute_bash",
        "tool_input": {"command": "ls"},
    })
    .to_string();
    let events = parse_kiro(&pre).unwrap();
    match &events[0] {
        ParsedHookEvent::PreBashCall(e) => {
            assert_eq!(e.context.agent_id.tool, "kiro");
            assert_eq!(e.tool_use_id, "bash");
        }
        _ => panic!("Expected PreBashCall"),
    }

    let post = json!({
        "hook_event_name": "postToolUse",
        "cwd": "/tmp/proj",
        "session_id": "sess-3",
        "tool_name": "shell",
        "tool_input": {"command": "ls"},
    })
    .to_string();
    let events = parse_kiro(&post).unwrap();
    match &events[0] {
        ParsedHookEvent::PostBashCall(e) => {
            assert_eq!(e.context.agent_id.tool, "kiro");
            assert!(e.transcript_source.is_none());
        }
        _ => panic!("Expected PostBashCall"),
    }
}

#[test]
fn test_kiro_rejects_lifecycle_events() {
    for event in ["agentSpawn", "userPromptSubmit", "stop"] {
        let payload = json!({
            "hook_event_name": event,
            "cwd": "/tmp",
            "session_id": "sess-rej",
        })
        .to_string();
        let result = parse_kiro(&payload);
        assert!(
            result.is_err(),
            "expected error for lifecycle event {event}, got Ok"
        );
    }
}

#[test]
fn test_kiro_rejects_pascal_case_event_names() {
    // Claude-shaped PascalCase event names must NOT match.
    for event in ["PreToolUse", "PostToolUse"] {
        let payload = json!({
            "hook_event_name": event,
            "cwd": "/tmp",
            "session_id": "sess-rej",
            "tool_name": "fs_write",
            "tool_input": {"path": "x"},
        })
        .to_string();
        let result = parse_kiro(&payload);
        assert!(
            result.is_err(),
            "PascalCase {event} should not match Kiro's camelCase convention"
        );
    }
}

#[test]
fn test_kiro_rejects_unsupported_tools() {
    for tool in ["fs_read", "read", "use_aws", "aws", "@git/status"] {
        let payload = json!({
            "hook_event_name": "postToolUse",
            "cwd": "/tmp",
            "session_id": "sess-tool",
            "tool_name": tool,
            "tool_input": {},
        })
        .to_string();
        let result = parse_kiro(&payload);
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
fn test_kiro_e2e_fs_write_attributes_to_kiro() {
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
        "hook_event_name": "postToolUse",
        "cwd": canonical_root.to_string_lossy().to_string(),
        "session_id": "kiro-e2e-1",
        "tool_name": "fs_write",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "def hello():\n    pass\ndef world():\n    pass\n",
        },
    })
    .to_string();

    repo.git_ai(&["checkpoint", "kiro", "--hook-input", &hook_input])
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
        "Should have AI attestations from Kiro"
    );
    let session = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("session record should exist");
    assert_eq!(session.agent_id.tool, "kiro");
    assert_eq!(session.agent_id.id, "kiro-e2e-1");
    assert_eq!(
        session.agent_id.model, "unknown",
        "model defaults to 'unknown' since hook payload doesn't expose it"
    );
}

#[test]
fn test_kiro_e2e_pre_then_post_isolates_human_lines() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("index.ts");
    fs::write(&file_path, "console.log('hello');\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let canonical_root = repo.canonical_path();
    let canonical_file = canonical_root.join("index.ts");

    let pre = json!({
        "hook_event_name": "preToolUse",
        "cwd": canonical_root.to_string_lossy().to_string(),
        "session_id": "kiro-e2e-2",
        "tool_name": "fs_write",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "console.log('hello');\nconsole.log('from kiro');\n",
        },
    })
    .to_string();
    repo.git_ai(&["checkpoint", "kiro", "--hook-input", &pre])
        .unwrap();

    fs::write(
        &file_path,
        "console.log('hello');\nconsole.log('from kiro');\n",
    )
    .unwrap();

    let post = json!({
        "hook_event_name": "postToolUse",
        "cwd": canonical_root.to_string_lossy().to_string(),
        "session_id": "kiro-e2e-2",
        "tool_name": "fs_write",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "console.log('hello');\nconsole.log('from kiro');\n",
        },
    })
    .to_string();
    repo.git_ai(&["checkpoint", "kiro", "--hook-input", &post])
        .unwrap();

    let commit = repo
        .stage_all_and_commit("Add kiro line")
        .expect("commit should succeed");

    let mut file = repo.filename("index.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('hello');".human(),
        "console.log('from kiro');".ai(),
    ]);

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Should have attestations"
    );
}

#[test]
fn test_kiro_e2e_write_alias_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("greet.py");
    fs::write(&file_path, "def greet():\n    print('hi')\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(&file_path, "def greet():\n    print('hello world')\n").unwrap();

    let canonical_root = repo.canonical_path();
    let canonical_file = canonical_root.join("greet.py");
    let hook_input = json!({
        "hook_event_name": "postToolUse",
        "cwd": canonical_root.to_string_lossy().to_string(),
        "session_id": "kiro-e2e-3",
        "tool_name": "write",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "def greet():\n    print('hello world')\n",
        },
    })
    .to_string();

    repo.git_ai(&["checkpoint", "kiro", "--hook-input", &hook_input])
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
