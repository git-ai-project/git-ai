//! Integration tests for the Kimi Code (Moonshot AI) preset.
//!
//! Exercises the full end-to-end flow: real `git-ai checkpoint kimi-code`
//! invocation through the test binary, real `TestRepo`, real attribution
//! checks against produced authorship notes.
//!
//! The hook payload schema mirrors what kimi-cli sends (verified against
//! `MoonshotAI/kimi-cli/src/kimi_cli/hooks/events.py`):
//!   - top-level: `hook_event_name`, `session_id`, `cwd`
//!   - per-event: `tool_name`, `tool_input`, `tool_call_id`
//!   - tool_input: `path` for `WriteFile`/`StrReplaceFile`; `command` for `Shell`

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;

fn parse_kimi(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("kimi-code")?.parse(hook_input, "t_test123456789a")
}

// ============================================================================
// Preset routing tests (in-process)
// ============================================================================

#[test]
fn test_kimi_code_preset_resolves() {
    assert!(
        resolve_preset("kimi-code").is_ok(),
        "kimi-code preset must be registered in resolve_preset"
    );
}

#[test]
fn test_kimi_code_preset_routes_writefile_to_post_file_edit() {
    let hook_input = json!({
        "session_id": "ki-1",
        "cwd": "/tmp/proj",
        "hook_event_name": "PostToolUse",
        "tool_name": "WriteFile",
        "tool_call_id": "tc-write-1",
        "tool_input": {"path": "/tmp/proj/main.rs", "content": "fn main() {}", "mode": "overwrite"},
        "tool_output": "wrote 14 bytes"
    })
    .to_string();
    let events = parse_kimi(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "kimi-code");
            assert_eq!(e.context.agent_id.id, "ki-1");
            assert_eq!(e.context.agent_id.model, "unknown");
            assert!(
                e.transcript_source.is_none(),
                "transcript_source should be None until a Kimi reader lands"
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_kimi_code_preset_routes_strreplacefile_to_post_file_edit() {
    let hook_input = json!({
        "session_id": "ki-2",
        "cwd": "/tmp/proj",
        "hook_event_name": "PostToolUse",
        "tool_name": "StrReplaceFile",
        "tool_call_id": "tc-edit-1",
        "tool_input": {"path": "/tmp/proj/lib.rs", "edit": {"old": "a", "new": "b", "replace_all": false}}
    })
    .to_string();
    let events = parse_kimi(&hook_input).unwrap();
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
fn test_kimi_code_preset_routes_pretooluse_to_pre_file_edit() {
    let hook_input = json!({
        "session_id": "ki-3",
        "cwd": "/tmp/proj",
        "hook_event_name": "PreToolUse",
        "tool_name": "WriteFile",
        "tool_call_id": "tc-1",
        "tool_input": {"path": "/tmp/proj/foo.rs", "content": "x", "mode": "overwrite"}
    })
    .to_string();
    let events = parse_kimi(&hook_input).unwrap();
    assert!(matches!(events[0], ParsedHookEvent::PreFileEdit(_)));
}

#[test]
fn test_kimi_code_preset_routes_shell_pretooluse_to_pre_bash_call() {
    let hook_input = json!({
        "session_id": "ki-4",
        "cwd": "/tmp/proj",
        "hook_event_name": "PreToolUse",
        "tool_name": "Shell",
        "tool_call_id": "tc-shell-1",
        "tool_input": {"command": "ls", "timeout": 30, "run_in_background": false, "description": ""}
    })
    .to_string();
    let events = parse_kimi(&hook_input).unwrap();
    match &events[0] {
        ParsedHookEvent::PreBashCall(e) => {
            assert_eq!(e.tool_use_id, "tc-shell-1");
        }
        _ => panic!("Expected PreBashCall"),
    }
}

#[test]
fn test_kimi_code_preset_routes_shell_posttooluse_to_post_bash_call() {
    let hook_input = json!({
        "session_id": "ki-5",
        "cwd": "/tmp/proj",
        "hook_event_name": "PostToolUse",
        "tool_name": "Shell",
        "tool_call_id": "tc-shell-2",
        "tool_input": {"command": "ls"},
        "tool_output": "src\n"
    })
    .to_string();
    let events = parse_kimi(&hook_input).unwrap();
    match &events[0] {
        ParsedHookEvent::PostBashCall(e) => {
            assert_eq!(e.tool_use_id, "tc-shell-2");
            assert!(e.transcript_source.is_none());
        }
        _ => panic!("Expected PostBashCall"),
    }
}

#[test]
fn test_kimi_code_preset_rejects_lifecycle_events() {
    // SessionStart, Stop, SubagentStart, etc. must not silently fall through
    // and produce phantom file-edit checkpoints. PostToolUseFailure also
    // hits this branch — see preset module doc-comment for rationale.
    for event in [
        "PostToolUseFailure",
        "SessionStart",
        "SessionEnd",
        "Stop",
        "StopFailure",
        "SubagentStart",
        "SubagentStop",
        "UserPromptSubmit",
        "PreCompact",
        "PostCompact",
        "Notification",
    ] {
        let payload = json!({
            "session_id": "ki-rej",
            "cwd": "/tmp/proj",
            "hook_event_name": event,
        })
        .to_string();
        let result = parse_kimi(&payload);
        assert!(
            result.is_err(),
            "expected error for lifecycle event {event}, got Ok"
        );
    }
}

#[test]
fn test_kimi_code_preset_rejects_unsupported_tools() {
    for tool in [
        "ReadFile",
        "Grep",
        "Glob",
        "SearchWeb",
        "FetchURL",
        "LaborMarket",
    ] {
        let payload = json!({
            "session_id": "ki-tool",
            "cwd": "/tmp/proj",
            "hook_event_name": "PostToolUse",
            "tool_name": tool,
            "tool_call_id": "tc",
            "tool_input": {},
        })
        .to_string();
        let result = parse_kimi(&payload);
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
fn test_kimi_code_e2e_writefile_attributes_to_kimi_code() {
    let repo = TestRepo::new();

    let file_path = repo.path().join("app.py");
    fs::write(&file_path, "def hello():\n    pass\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(
        &file_path,
        "def hello():\n    pass\ndef world():\n    pass\n",
    )
    .unwrap();

    let canonical_file = repo.canonical_path().join("app.py");
    let hook_input = json!({
        "session_id": "kimi-e2e-1",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "tool_name": "WriteFile",
        "tool_call_id": "tc-e2e-1",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "def hello():\n    pass\ndef world():\n    pass\n",
            "mode": "overwrite"
        },
        "tool_output": "wrote 47 bytes"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "kimi-code", "--hook-input", &hook_input])
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
        "Should have AI attestations from Kimi Code"
    );
    assert!(
        !commit.authorship_log.metadata.sessions.is_empty(),
        "Should have a session record"
    );

    let session_record = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("Should have a session record");

    assert_eq!(session_record.agent_id.tool, "kimi-code");
    assert_eq!(session_record.agent_id.id, "kimi-e2e-1");
    assert_eq!(
        session_record.agent_id.model, "unknown",
        "kimi-cli does not send model in hook payload; default to 'unknown'"
    );
}

#[test]
fn test_kimi_code_e2e_pre_then_post_tracks_only_ai_lines() {
    // PreToolUse fires before the AI writes — captures the human baseline.
    // PostToolUse after the AI write should attribute *only* the AI-added
    // lines as AI, leaving the existing line as human.
    let repo = TestRepo::new();

    let file_path = repo.path().join("index.ts");
    fs::write(&file_path, "console.log('hello');\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let canonical_file = repo.canonical_path().join("index.ts");
    let pre_hook_input = json!({
        "session_id": "kimi-e2e-2",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "tool_name": "WriteFile",
        "tool_call_id": "tc-e2e-2",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "console.log('hello');\nconsole.log('from kimi');\n",
            "mode": "overwrite"
        }
    })
    .to_string();

    repo.git_ai(&["checkpoint", "kimi-code", "--hook-input", &pre_hook_input])
        .unwrap();

    fs::write(
        &file_path,
        "console.log('hello');\nconsole.log('from kimi');\n",
    )
    .unwrap();

    let post_hook_input = json!({
        "session_id": "kimi-e2e-2",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "tool_name": "WriteFile",
        "tool_call_id": "tc-e2e-2",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "content": "console.log('hello');\nconsole.log('from kimi');\n",
            "mode": "overwrite"
        },
        "tool_output": "wrote 47 bytes"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "kimi-code", "--hook-input", &post_hook_input])
        .unwrap();

    let commit = repo
        .stage_all_and_commit("Add kimi line")
        .expect("commit should succeed");

    let mut file = repo.filename("index.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('hello');".human(),
        "console.log('from kimi');".ai(),
    ]);

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Should have attestations"
    );
}

#[test]
fn test_kimi_code_e2e_strreplacefile_attributes_changed_lines() {
    let repo = TestRepo::new();

    let file_path = repo.path().join("greet.py");
    fs::write(&file_path, "def greet():\n    print('hi')\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(&file_path, "def greet():\n    print('hello world')\n").unwrap();

    let canonical_file = repo.canonical_path().join("greet.py");
    let hook_input = json!({
        "session_id": "kimi-e2e-3",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "tool_name": "StrReplaceFile",
        "tool_call_id": "tc-e2e-3",
        "tool_input": {
            "path": canonical_file.to_string_lossy().to_string(),
            "edit": {"old": "    print('hi')", "new": "    print('hello world')", "replace_all": false}
        }
    })
    .to_string();

    repo.git_ai(&["checkpoint", "kimi-code", "--hook-input", &hook_input])
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
