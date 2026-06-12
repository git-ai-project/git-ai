//! Integration tests for the Cline (cline.bot VS Code extension) preset.
//!
//! Exercises the full flow: real `git-ai checkpoint cline` invocation
//! through the test binary, real `TestRepo`, real attribution checks
//! against produced authorship notes.
//!
//! The hook payload schema mirrors what Cline sends per
//! <https://docs.cline.bot/customization/hooks>:
//!   - top-level: `taskId`, `hookName`, `clineVersion`, `timestamp`,
//!     `workspaceRoots[]`, `userId`, `model`, `toolName`, `parameters`
//!   - parameters: `path` for `write_to_file`/`replace_in_file`,
//!     `command` for `execute_command`

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;

fn parse_cline(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("cline")?.parse(hook_input, "t_test123456789a")
}

#[test]
fn test_cline_preset_resolves() {
    assert!(
        resolve_preset("cline").is_ok(),
        "cline preset must be registered in resolve_preset"
    );
}

#[test]
fn test_cline_routes_write_to_file_to_post_file_edit() {
    let hook_input = json!({
        "taskId": "task-1",
        "hookName": "PostToolUse",
        "clineVersion": "3.17.0",
        "workspaceRoots": ["/tmp/proj"],
        "model": {"provider": "anthropic", "slug": "claude-sonnet-4-5"},
        "toolName": "write_to_file",
        "parameters": {"path": "/tmp/proj/main.rs", "content": "fn main() {}"},
    })
    .to_string();
    let events = parse_cline(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "cline");
            assert_eq!(e.context.agent_id.id, "task-1");
            assert_eq!(e.context.agent_id.model, "claude-sonnet-4-5");
            assert!(
                e.transcript_source.is_none(),
                "transcript_source should be None until a Cline reader lands"
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_cline_routes_replace_in_file_to_post_file_edit() {
    let hook_input = json!({
        "taskId": "task-2",
        "hookName": "PostToolUse",
        "workspaceRoots": ["/tmp/proj"],
        "toolName": "replace_in_file",
        "parameters": {"path": "/tmp/proj/lib.rs", "diff": "..."},
    })
    .to_string();
    let events = parse_cline(&hook_input).unwrap();
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
fn test_cline_routes_execute_command_to_bash() {
    let pre = json!({
        "taskId": "task-3",
        "hookName": "PreToolUse",
        "workspaceRoots": ["/tmp/proj"],
        "toolName": "execute_command",
        "parameters": {"command": "ls", "requires_approval": false},
    })
    .to_string();
    let events = parse_cline(&pre).unwrap();
    match &events[0] {
        ParsedHookEvent::PreBashCall(e) => {
            assert_eq!(e.context.agent_id.tool, "cline");
            assert_eq!(e.tool_use_id, "bash");
        }
        _ => panic!("Expected PreBashCall"),
    }

    let post = json!({
        "taskId": "task-3",
        "hookName": "PostToolUse",
        "workspaceRoots": ["/tmp/proj"],
        "toolName": "execute_command",
        "parameters": {"command": "ls"},
    })
    .to_string();
    let events = parse_cline(&post).unwrap();
    match &events[0] {
        ParsedHookEvent::PostBashCall(e) => {
            assert_eq!(e.context.agent_id.tool, "cline");
            assert!(e.transcript_source.is_none());
        }
        _ => panic!("Expected PostBashCall"),
    }
}

#[test]
fn test_cline_rejects_lifecycle_events() {
    for event in [
        "TaskStart",
        "TaskResume",
        "TaskCancel",
        "TaskComplete",
        "UserPromptSubmit",
        "PreCompact",
    ] {
        let payload = json!({
            "taskId": "task-rej",
            "hookName": event,
            "workspaceRoots": ["/tmp/proj"],
        })
        .to_string();
        let result = parse_cline(&payload);
        assert!(
            result.is_err(),
            "expected error for lifecycle event {event}, got Ok"
        );
    }
}

#[test]
fn test_cline_rejects_unsupported_tools() {
    for tool in [
        "read_file",
        "search_files",
        "list_files",
        "list_code_definition_names",
        "browser_action",
        "use_mcp_tool",
        "access_mcp_resource",
        "ask_followup_question",
        "attempt_completion",
        "new_task",
    ] {
        let payload = json!({
            "taskId": "task-tool",
            "hookName": "PostToolUse",
            "workspaceRoots": ["/tmp/proj"],
            "toolName": tool,
            "parameters": {},
        })
        .to_string();
        let result = parse_cline(&payload);
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
fn test_cline_e2e_write_to_file_attributes_to_cline() {
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
        "taskId": "cline-e2e-1",
        "hookName": "PostToolUse",
        "clineVersion": "3.17.0",
        "workspaceRoots": [canonical_root.to_string_lossy().to_string()],
        "model": {"provider": "anthropic", "slug": "claude-sonnet-4-5"},
        "toolName": "write_to_file",
        "parameters": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "def hello():\n    pass\ndef world():\n    pass\n",
        },
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cline", "--hook-input", &hook_input])
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
        "Should have AI attestations from Cline"
    );
    let session = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("session record should exist");
    assert_eq!(session.agent_id.tool, "cline");
    assert_eq!(session.agent_id.id, "cline-e2e-1");
    assert_eq!(session.agent_id.model, "claude-sonnet-4-5");
}

#[test]
fn test_cline_e2e_pre_then_post_isolates_human_lines() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("index.ts");
    fs::write(&file_path, "console.log('hello');\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let canonical_root = repo.canonical_path();
    let canonical_file = canonical_root.join("index.ts");

    let pre = json!({
        "taskId": "cline-e2e-2",
        "hookName": "PreToolUse",
        "workspaceRoots": [canonical_root.to_string_lossy().to_string()],
        "model": {"provider": "anthropic", "slug": "claude-sonnet-4-5"},
        "toolName": "write_to_file",
        "parameters": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "console.log('hello');\nconsole.log('from cline');\n",
        },
    })
    .to_string();
    repo.git_ai(&["checkpoint", "cline", "--hook-input", &pre])
        .unwrap();

    fs::write(
        &file_path,
        "console.log('hello');\nconsole.log('from cline');\n",
    )
    .unwrap();

    let post = json!({
        "taskId": "cline-e2e-2",
        "hookName": "PostToolUse",
        "workspaceRoots": [canonical_root.to_string_lossy().to_string()],
        "model": {"provider": "anthropic", "slug": "claude-sonnet-4-5"},
        "toolName": "write_to_file",
        "parameters": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "console.log('hello');\nconsole.log('from cline');\n",
        },
    })
    .to_string();
    repo.git_ai(&["checkpoint", "cline", "--hook-input", &post])
        .unwrap();

    let commit = repo
        .stage_all_and_commit("Add cline line")
        .expect("commit should succeed");

    let mut file = repo.filename("index.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('hello');".human(),
        "console.log('from cline');".ai(),
    ]);

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Should have attestations"
    );
}

#[test]
fn test_cline_e2e_replace_in_file_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("greet.py");
    fs::write(&file_path, "def greet():\n    print('hi')\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(&file_path, "def greet():\n    print('hello world')\n").unwrap();

    let canonical_root = repo.canonical_path();
    let canonical_file = canonical_root.join("greet.py");
    let hook_input = json!({
        "taskId": "cline-e2e-3",
        "hookName": "PostToolUse",
        "workspaceRoots": [canonical_root.to_string_lossy().to_string()],
        "model": {"provider": "anthropic", "slug": "claude-sonnet-4-5"},
        "toolName": "replace_in_file",
        "parameters": {
            "path": canonical_file.to_string_lossy().to_string(),
            "diff": "@@ -2 +2 @@\n-    print('hi')\n+    print('hello world')\n",
        },
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cline", "--hook-input", &hook_input])
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
