//! Integration tests for the Augment Code (Auggie) preset: real
//! `git-ai checkpoint augment` invocations through the test binary, a real
//! `TestRepo`, and real attribution checks against produced authorship
//! notes, for both the v1 (`auggie`) and v2 (`auggie-v2`/`cosmos-agent`)
//! hook payload shapes.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;

fn parse_augment(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("augment")
        .unwrap()
        .parse(hook_input, "t_test")
}

#[test]
fn test_augment_preset_resolves() {
    assert!(resolve_preset("augment").is_ok());
}

#[test]
fn test_augment_lifecycle_and_unsupported_tools_are_silent_no_ops_via_real_binary() {
    // Exercised through the real compiled binary (not just the in-process
    // parser) so a caller relying on `git-ai checkpoint` exiting 0 with no
    // diagnostic output for these cases is protected end-to-end: the
    // installer's catch-all ".*" matcher fires this hook for every event,
    // and Augment renders any exit-0 stderr as a user-visible warning.
    let repo = TestRepo::new();

    let lifecycle = json!({
        "hook_event_name": "SessionEnd",
        "conversation_id": "conv-1",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
    })
    .to_string();
    let output = repo
        .git_ai(&["checkpoint", "augment", "--hook-input", &lifecycle])
        .expect("must exit 0 for a lifecycle-event skip");
    assert!(output.trim().is_empty(), "got: {output:?}");

    let read_tool = json!({
        "hook_type": "PreToolUse",
        "tool_name": "read",
        "tool_input": {"path": "foo.txt"},
    })
    .to_string();
    let output = repo
        .git_ai(&["checkpoint", "augment", "--hook-input", &read_tool])
        .expect("must exit 0 for a v2 read-tool skip");
    assert!(output.trim().is_empty(), "got: {output:?}");
}

#[test]
fn test_augment_handler_reports_actionable_stderr_for_malformed_input() {
    let repo = TestRepo::new();

    let cases: &[(&str, &str)] = &[
        ("not valid json", "Invalid JSON in hook_input"),
        (
            r#"{"hook_event_name":"PostToolUse","hook_type":"PostToolUse","conversation_id":"c","workspace_roots":["/x"]}"#,
            "Ambiguous Augment hook_input",
        ),
        (
            r#"{"hook_event_name":123,"conversation_id":"c","workspace_roots":["/x"]}"#,
            "hook_event_name must be a string",
        ),
    ];
    for (input, expected_substring) in cases {
        let output = repo
            .git_ai(&["checkpoint", "augment", "--hook-input", input])
            .expect("git-ai checkpoint always exits 0, even on a preset error");
        assert!(
            output.contains("augment preset error") && output.contains(expected_substring),
            "input={input}, got: {output:?}"
        );
    }
}

#[test]
fn test_augment_v1_e2e_checkpoint_attributes_ai_line_to_augment() {
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

    assert!(!commit.authorship_log.attestations.is_empty());
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
fn test_augment_v2_e2e_checkpoint_attributes_ai_line_to_augment() {
    // v2 never sends workspace_roots; the hook subprocess's own cwd IS the
    // workspace root, which TestRepo::git_ai sets via Command::current_dir.
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.rs");
    fs::write(&file_path, "fn hello() {}\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(&file_path, "fn hello() {}\nfn world() {}\n").unwrap();

    let hook_input = json!({
        "hook_type": "PostToolUse",
        "tool_name": "write",
        "tool_input": {"path": "main.rs", "content": "fn hello() {}\nfn world() {}\n"},
        "tool_result": [{"type": "text", "text": "Successfully wrote main.rs"}],
        "tool_is_error": false,
    })
    .to_string();
    repo.git_ai(&["checkpoint", "augment", "--hook-input", &hook_input])
        .unwrap();

    let commit = repo
        .stage_all_and_commit("Add world function via v2")
        .expect("commit should succeed");

    let mut file = repo.filename("main.rs");
    file.assert_lines_and_blame(crate::lines!["fn hello() {}".human(), "fn world() {}".ai(),]);

    assert!(!commit.authorship_log.attestations.is_empty());
    let session = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("session record should exist");
    assert_eq!(session.agent_id.tool, "augment");
    assert_eq!(
        session.agent_id.model, "unknown",
        "v2 never mines ~/.augment/sessions-v2 for model metadata"
    );
}

#[test]
fn test_augment_v2_terminal_tool_detects_bash_changes() {
    // "terminal" is the documented current v2 shell tool name
    // (docs.augmentcode.com/cli/permissions); a v2 PostToolUse payload for
    // it must produce a bash checkpoint and attribute the resulting file
    // change to AI, exactly like "bash" does. Mirrors
    // `test_gemini_preset_bash_tool_aftertool_detects_changes`'s
    // before/after shell-call shape.
    let repo = TestRepo::new();
    let file_path = repo.path().join("output.txt");
    fs::write(&file_path, "original\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let pre_hook_input = json!({
        "hook_type": "PreToolUse",
        "tool_name": "terminal",
        "tool_input": {"command": "echo modified > output.txt"},
    })
    .to_string();
    repo.git_ai(&["checkpoint", "augment", "--hook-input", &pre_hook_input])
        .unwrap();

    fs::write(&file_path, "modified\n").unwrap();

    let post_hook_input = json!({
        "hook_type": "PostToolUse",
        "tool_name": "terminal",
        "tool_input": {"command": "echo modified > output.txt"},
        "tool_output": "",
        "tool_is_error": false,
    })
    .to_string();
    repo.git_ai(&["checkpoint", "augment", "--hook-input", &post_hook_input])
        .unwrap();

    let commit = repo.stage_all_and_commit("Modify via v2 terminal").unwrap();
    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "v2 PostToolUse with tool_name=terminal should produce AI attestations"
    );
    let session = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("session record should exist");
    assert_eq!(session.agent_id.tool, "augment");
}

// ============================================================================
// In-process parser matrix tests (autodetection + tool routing), exercising
// the same `resolve_preset("augment")` entry point the CLI handler uses.
// ============================================================================

#[test]
fn test_augment_autodetect_matrix_via_resolved_preset() {
    // v1-only key.
    let v1 = json!({
        "hook_event_name": "PostToolUse",
        "conversation_id": "conv-1",
        "workspace_roots": ["/tmp/proj"],
        "tool_name": "save-file",
        "tool_input": {"path": "a.rs"},
    })
    .to_string();
    assert!(matches!(
        parse_augment(&v1).unwrap()[0],
        ParsedHookEvent::PostFileEdit(_)
    ));

    // v2-only key.
    let v2 = json!({
        "hook_type": "PostToolUse",
        "tool_name": "write",
        "tool_input": {"path": "a.rs"},
    })
    .to_string();
    assert!(matches!(
        parse_augment(&v2).unwrap()[0],
        ParsedHookEvent::PostFileEdit(_)
    ));

    // Neither key -> error.
    let neither = json!({"tool_name": "write"}).to_string();
    assert!(parse_augment(&neither).is_err());
}

#[test]
fn test_augment_v1_routes_all_mutating_tools_to_expected_class() {
    let cases: &[(&str, serde_json::Value, bool)] = &[
        ("save-file", json!({"path": "a.rs", "content": "x"}), false),
        ("str-replace-editor", json!({"path": "a.rs"}), false),
        (
            "remove-files",
            json!({"file_paths": ["a.rs", "b.rs"]}),
            false,
        ),
        ("launch-process", json!({"command": "ls"}), true),
    ];
    for (tool, tool_input, expect_bash) in cases {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "conversation_id": "conv-1",
            "workspace_roots": ["/tmp/proj"],
            "tool_name": tool,
            "tool_input": tool_input,
        })
        .to_string();
        let events = parse_augment(&input).unwrap();
        assert_eq!(events.len(), 1, "tool={tool}");
        if *expect_bash {
            assert!(
                matches!(events[0], ParsedHookEvent::PostBashCall(_)),
                "tool={tool}"
            );
        } else {
            assert!(
                matches!(events[0], ParsedHookEvent::PostFileEdit(_)),
                "tool={tool}"
            );
        }
    }
}

// Installer command shell-safety is covered by
// `mdm::agents::augment::tests::desired_command_quoting_matrix`, which
// pins the exact quoted command string (including its lack of shell
// metacharacters) for plain, spacey, and Windows-style binary paths.
