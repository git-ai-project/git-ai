//! Kilo Code (CLI and VS Code extension) attribution.
//!
//! Kilo is an OpenCode fork: it calls the OpenCode V1 plugin API and stores
//! sessions in an OpenCode-schema SQLite database named `kilo.db` under
//! `~/.local/share/kilo`. These tests reuse the OpenCode fixtures under Kilo's
//! names to prove edits are attributed to `kilo` with the session's model.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use crate::test_utils::fixture_path;
use git_ai::authorship::authorship_log_serialization::generate_session_id;
use git_ai::mdm::agents::{KiloInstaller, OpenCodePluginAgent};
use serde_json::json;
use std::fs;
use std::process::Command;

fn kilo_storage_with_fixture_db(dir: &std::path::Path) -> std::path::PathBuf {
    let storage = dir.join("kilo-storage");
    fs::create_dir_all(&storage).unwrap();
    fs::copy(
        fixture_path("opencode-sqlite").join("opencode.db"),
        storage.join("kilo.db"),
    )
    .unwrap();
    storage
}

/// The `kilo` CLI keeps sessions in a per-channel DB (`kilo-<channel>.db`);
/// only the VS Code extension forces `kilo.db` via KILO_DISABLE_CHANNEL_DB.
/// Both exist side by side, and only the channel DB has the CLI's session.
fn kilo_storage_with_channel_db(dir: &std::path::Path) -> std::path::PathBuf {
    let storage = dir.join("kilo-storage");
    fs::create_dir_all(&storage).unwrap();
    fs::copy(
        fixture_path("opencode-sqlite-empty").join("opencode.db"),
        storage.join("kilo.db"),
    )
    .unwrap();
    fs::copy(
        fixture_path("opencode-sqlite").join("opencode.db"),
        storage.join("kilo-rc.db"),
    )
    .unwrap();
    storage
}

/// Kilo keeps writing to a pre-rename `opencode-<channel>.db` in its own data
/// dir when no `kilo-<channel>.db` exists yet.
fn kilo_storage_with_legacy_channel_db(dir: &std::path::Path) -> std::path::PathBuf {
    let storage = dir.join("kilo-storage");
    fs::create_dir_all(&storage).unwrap();
    fs::copy(
        fixture_path("opencode-sqlite").join("opencode.db"),
        storage.join("opencode-rc.db"),
    )
    .unwrap();
    storage
}

/// `KILO_DB` points Kilo at a database of any name and location.
fn kilo_storage_with_kilo_db_override(dir: &std::path::Path) -> std::path::PathBuf {
    let storage = dir.join("kilo-storage");
    fs::create_dir_all(&storage).unwrap();
    fs::copy(
        fixture_path("opencode-sqlite-empty").join("opencode.db"),
        storage.join("kilo.db"),
    )
    .unwrap();
    let custom = dir.join("elsewhere").join("sessions.sqlite");
    fs::create_dir_all(custom.parent().unwrap()).unwrap();
    fs::copy(fixture_path("opencode-sqlite").join("opencode.db"), &custom).unwrap();
    // SAFETY: serialized test; the checkpoint subprocess inherits this env.
    unsafe {
        std::env::set_var("KILO_DB", &custom);
    }
    storage
}

#[test]
#[serial_test::serial]
fn test_kilo_legacy_channel_db_records_model() {
    assert_kilo_checkpoint_records_model(kilo_storage_with_legacy_channel_db);
}

#[test]
#[serial_test::serial]
fn test_kilo_db_override_records_model() {
    assert_kilo_checkpoint_records_model(kilo_storage_with_kilo_db_override);
    // SAFETY: serialized test.
    unsafe {
        std::env::remove_var("KILO_DB");
    }
}

#[test]
#[serial_test::serial]
fn test_kilo_checkpoint_attributes_lines_to_kilo_with_model() {
    assert_kilo_checkpoint_records_model(kilo_storage_with_fixture_db);
}

#[test]
#[serial_test::serial]
fn test_kilo_cli_channel_db_records_model() {
    assert_kilo_checkpoint_records_model(kilo_storage_with_channel_db);
}

fn assert_kilo_checkpoint_records_model(storage_for: fn(&std::path::Path) -> std::path::PathBuf) {
    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });
    let root = repo.canonical_path();
    let file_path = root.join("main.ts");
    fs::write(&file_path, "// initial\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let temp = tempfile::tempdir().unwrap();
    let storage = storage_for(temp.path());
    // SAFETY: serialized test; the checkpoint subprocess inherits this env.
    unsafe {
        std::env::set_var("GIT_AI_KILO_STORAGE_PATH", &storage);
    }

    let hook = |event: &str| {
        json!({
            "hook_event_name": event,
            "session_id": "test-session-123",
            "tool_use_id": "call-1",
            "tool_name": "edit",
            "cwd": root.to_string_lossy(),
            "tool_input": { "file_paths": [file_path.to_string_lossy()] },
        })
        .to_string()
    };
    repo.git_ai(&["checkpoint", "kilo", "--hook-input", &hook("PreToolUse")])
        .unwrap();
    fs::write(&file_path, "// initial\n// from kilo\n").unwrap();
    repo.git_ai(&["checkpoint", "kilo", "--hook-input", &hook("PostToolUse")])
        .unwrap();

    unsafe {
        std::env::remove_var("GIT_AI_KILO_STORAGE_PATH");
    }

    let commit = repo.stage_all_and_commit("Kilo edit").unwrap();
    repo.filename("main.ts")
        .assert_committed_lines(crate::lines![
            "// initial".unattributed_human(),
            "// from kilo".ai(),
        ]);

    let session = commit
        .authorship_log
        .metadata
        .sessions
        .get(&generate_session_id("test-session-123", "kilo"))
        .expect("kilo session record");
    assert_eq!(session.agent_id.tool, "kilo");
    assert_eq!(session.agent_id.model, "gpt-5");
}

#[test]
#[serial_test::serial]
fn test_kilo_plugin_v1_hooks_attribute_edits_to_kilo() {
    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });
    let root = repo.canonical_path();
    for name in ["first.ts", "second.ts", "unrelated.ts"] {
        fs::write(root.join(name), "// baseline\n").unwrap();
    }
    repo.stage_all_and_commit("Initial files").unwrap();

    fs::write(root.join("first.ts"), "// baseline\n// manual first\n").unwrap();
    fs::write(root.join("second.ts"), "// baseline\n// manual second\n").unwrap();
    repo.git_ai(&[
        "checkpoint",
        "mock_known_human",
        root.join("first.ts").to_str().unwrap(),
        root.join("second.ts").to_str().unwrap(),
    ])
    .unwrap();

    let isolated = tempfile::tempdir().unwrap();
    let command_env = repo.git_ai_command_without_pre_sync_for_test(&[], &[]);
    let binary_path = std::path::PathBuf::from(command_env.get_program());
    // Exactly what `git-ai install-hooks` writes to ~/.config/kilo/plugins/.
    let plugin_path = isolated.path().join("git-ai.mts");
    fs::write(
        &plugin_path,
        KiloInstaller::generate_plugin_content(&binary_path),
    )
    .unwrap();
    let storage = kilo_storage_with_fixture_db(isolated.path());

    let mut command = Command::new("node");
    for (key, value) in command_env.get_envs() {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    // Kilo (OpenCode 1.18.x base) calls the V1 `server()` entrypoint.
    let output = command
        .arg("--experimental-strip-types")
        .arg(fixture_path("opencode-plugin-harness.mjs"))
        .arg(&plugin_path)
        .arg(&root)
        .arg("v1")
        .arg(&root)
        .arg("default")
        .arg(&binary_path)
        .env("GIT_AI_KILO_STORAGE_PATH", &storage)
        .env("GIT_AI_OPENCODE_DEBUG", "1")
        .current_dir(isolated.path())
        .output()
        .expect("Kilo plugin test requires Node.js 22.6 or newer");
    assert!(
        output.status.success(),
        "Kilo plugin failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let commit = repo.stage_all_and_commit("Kilo plugin edits").unwrap();
    repo.filename("first.ts")
        .assert_committed_lines(crate::lines![
            "// baseline".unattributed_human(),
            "// manual first".human(),
            "// AI first".ai(),
            "// manual after".human(),
        ]);
    repo.filename("second.ts")
        .assert_committed_lines(crate::lines![
            "// baseline".unattributed_human(),
            "// manual second".human(),
            "// AI second".ai(),
        ]);

    let session = commit
        .authorship_log
        .metadata
        .sessions
        .get(&generate_session_id("test-session-123", "kilo"))
        .expect("kilo session record");
    assert_eq!(session.agent_id.tool, "kilo");
    assert_eq!(session.agent_id.model, "gpt-5");
}

/// The VS Code e2e flow: a human appends subtract() and saves (the git-ai
/// VS Code extension records a known-human checkpoint), then Kilo inserts
/// divide() above it. Only divide() is AI; known-human lines are never claimed
/// by post-commit edge recovery (which may claim up to three *unattested*
/// lines next to an AI block, see docs/bash-attribution-recovery-plan.md).
#[test]
fn test_kilo_insert_above_known_human_lines_keeps_them_human() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("calc.ts");
    let committed = "\
export function multiply(a: number, b: number): number {
  return a * b
}
";
    fs::write(&file_path, committed).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("calc.ts");
    file.assert_committed_lines(crate::lines![
        "export function multiply(a: number, b: number): number {".unattributed_human(),
        "  return a * b".unattributed_human(),
        "}".unattributed_human(),
    ]);

    let human = format!(
        "{committed}export function subtract(a: number, b: number): number {{\n  return a - b\n}}\n"
    );
    fs::write(&file_path, &human).unwrap();
    // Save in VS Code -> known human; then Kilo's pre-edit, edit and post-edit checkpoints.
    repo.git_ai(&["checkpoint", "mock_known_human", "calc.ts"])
        .unwrap();
    repo.git_ai(&["checkpoint", "human", "calc.ts"]).unwrap();
    let with_divide = human.replacen(
        "}\nexport function subtract",
        "}\n\nexport function divide(a: number, b: number): number {\n  if (b === 0) {\n    throw new Error('Division by zero')\n  }\n  return a / b\n}\nexport function subtract",
        1,
    );
    fs::write(&file_path, with_divide).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "calc.ts"]).unwrap();
    repo.stage_all_and_commit("Human subtract() + Kilo divide()")
        .unwrap();

    file.assert_committed_lines(crate::lines![
        "export function multiply(a: number, b: number): number {".unattributed_human(),
        "  return a * b".unattributed_human(),
        "}".unattributed_human(),
        "".ai(),
        "export function divide(a: number, b: number): number {".ai(),
        "  if (b === 0) {".ai(),
        "    throw new Error('Division by zero')".ai(),
        "  }".ai(),
        "  return a / b".ai(),
        "}".ai(),
        "export function subtract(a: number, b: number): number {".human(),
        "  return a - b".human(),
        "}".human(),
    ]);
}
