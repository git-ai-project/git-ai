use crate::repos::test_repo::TestRepo;
use serial_test::serial;
use std::fs;
use std::path::PathBuf;

struct EnvVarGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        // SAFETY: tests marked `serial` avoid concurrent env mutation.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, old }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: tests marked `serial` avoid concurrent env mutation.
        unsafe {
            if let Some(old) = &self.old {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn git_common_dir(repo: &TestRepo) -> PathBuf {
    let common_dir = repo
        .git(&["rev-parse", "--git-common-dir"])
        .expect("rev-parse --git-common-dir should succeed");
    let relative = common_dir.trim();
    repo.path().join(relative)
}

fn git_hooks_ai_dir(repo: &TestRepo) -> PathBuf {
    git_common_dir(repo).join("ai")
}

// =============================================================================
// Feature Flag 1: hooks_enabled
// When enabled, checkpoint's repo healing logic should always ensure hooks,
// even if hooks were not previously set up in the repo.
// =============================================================================

#[test]
#[serial]
fn hooks_enabled_flag_causes_checkpoint_to_ensure_hooks() {
    // Use wrapper mode so checkpoint runs through git-ai (not via git hooks)
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    let managed_hooks_dir = git_hooks_ai_dir(&repo).join("hooks");
    let marker_path = git_hooks_ai_dir(&repo).join("git_hooks_enabled"); // enablement marker file name

    // Verify hooks are NOT set up initially (no explicit git-hooks ensure was run
    // beyond what TestRepo::new does for wrapper mode, but wrapper mode doesn't
    // set up hooks by default when GIT_AI_TEST_GIT_MODE=wrapper only)
    // We'll remove any hooks that might have been set up to start clean
    let _ = repo.git_ai(&["git-hooks", "remove"]);

    assert!(
        !marker_path.exists(),
        "marker should not exist after removing hooks"
    );

    // Create a file, stage it, and run checkpoint with hooks_enabled flag via env
    fs::write(
        repo.path().join("flag-test.txt"),
        "hello from hooks_enabled flag test\n",
    )
    .expect("failed to write test file");
    repo.git(&["add", "flag-test.txt"])
        .expect("staging should succeed");

    // Run checkpoint with hooks_enabled feature flag set via environment variable
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "flag-test.txt"],
        &[("GIT_AI_GIT_HOOKS_ENABLED", "true")],
    )
    .expect("checkpoint with hooks_enabled should succeed");

    // The hooks_enabled flag should cause checkpoint to ensure hooks are installed
    assert!(
        managed_hooks_dir.exists(),
        "managed hooks directory should exist after checkpoint with hooks_enabled=true"
    );
}

#[test]
#[serial]
fn hooks_disabled_flag_does_not_ensure_hooks_on_clean_repo() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    let managed_hooks_dir = git_hooks_ai_dir(&repo).join("hooks");

    // Remove any hooks to start clean
    let _ = repo.git_ai(&["git-hooks", "remove"]);

    assert!(
        !managed_hooks_dir.exists() || managed_hooks_dir.symlink_metadata().is_err(),
        "managed hooks should not exist after remove"
    );

    // Create a file, stage it, and run checkpoint WITHOUT hooks_enabled flag
    fs::write(
        repo.path().join("no-flag-test.txt"),
        "hello without hooks_enabled flag\n",
    )
    .expect("failed to write test file");
    repo.git(&["add", "no-flag-test.txt"])
        .expect("staging should succeed");

    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "no-flag-test.txt"],
        &[("GIT_AI_GIT_HOOKS_ENABLED", "false")],
    )
    .expect("checkpoint without hooks_enabled should succeed");

    // Without the flag, hooks should NOT be installed on a clean repo
    assert!(
        !managed_hooks_dir.exists() || managed_hooks_dir.symlink_metadata().is_err(),
        "managed hooks should NOT be created when hooks_enabled is false on a clean repo"
    );
}

// =============================================================================
// Feature Flag 2: hooks_externally_managed
// When enabled (and hooks_enabled is also enabled): hooks are installed but
// git config (core.hooksPath) is NOT changed and forwarding is disabled.
// =============================================================================

#[test]
#[serial]
fn externally_managed_hooks_skips_git_config_change() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    // Remove any hooks to start clean
    let _ = repo.git_ai(&["git-hooks", "remove"]);

    // Verify no hooksPath is set initially
    let hooks_path_before = repo.git(&["config", "--local", "--get", "core.hooksPath"]);
    assert!(
        hooks_path_before.is_err(),
        "core.hooksPath should not be set initially after remove"
    );

    // Run git-hooks ensure with both flags enabled via env vars
    repo.git_ai_with_env(
        &["git-hooks", "ensure"],
        &[
            ("GIT_AI_GIT_HOOKS_ENABLED", "true"),
            ("GIT_AI_GIT_HOOKS_EXTERNALLY_MANAGED", "true"),
        ],
    )
    .expect("git-hooks ensure with externally_managed should succeed");

    let managed_hooks_dir = git_hooks_ai_dir(&repo).join("hooks");

    // Hook scripts should be installed in the managed directory
    assert!(
        managed_hooks_dir.exists(),
        "managed hooks directory should exist even with externally_managed"
    );

    // But core.hooksPath should NOT be changed
    let hooks_path_after = repo.git(&["config", "--local", "--get", "core.hooksPath"]);
    assert!(
        hooks_path_after.is_err(),
        "core.hooksPath should NOT be set when hooks_externally_managed is true"
    );
}

#[test]
#[serial]
fn externally_managed_hooks_disables_forwarding() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    // Set up a pre-existing hooks path that would normally be forwarded to
    let user_hooks_dir = git_common_dir(&repo).join("custom-hooks");
    fs::create_dir_all(&user_hooks_dir).expect("failed to create custom hooks dir");
    repo.git(&[
        "config",
        "--local",
        "core.hooksPath",
        user_hooks_dir.to_string_lossy().as_ref(),
    ])
    .expect("setting preexisting local hooksPath should succeed");

    // Run git-hooks ensure with both flags enabled
    repo.git_ai_with_env(
        &["git-hooks", "ensure"],
        &[
            ("GIT_AI_GIT_HOOKS_ENABLED", "true"),
            ("GIT_AI_GIT_HOOKS_EXTERNALLY_MANAGED", "true"),
        ],
    )
    .expect("git-hooks ensure with externally_managed should succeed");

    // Check that the repo hook state has ForwardMode::None (no forwarding)
    let state_path = git_hooks_ai_dir(&repo).join("git_hooks_state.json");
    assert!(
        state_path.exists(),
        "repo hook state should exist after ensure"
    );
    let state_content = fs::read_to_string(&state_path).expect("should be able to read state file");
    let state_json: serde_json::Value =
        serde_json::from_str(&state_content).expect("state should be valid JSON");

    assert_eq!(
        state_json["forward_mode"], "none",
        "forward_mode should be 'none' when hooks_externally_managed is true"
    );
    assert!(
        state_json["forward_hooks_path"].is_null(),
        "forward_hooks_path should be null when hooks_externally_managed is true"
    );

    // core.hooksPath should NOT have been changed to managed hooks path
    let hooks_path = repo
        .git(&["config", "--local", "--get", "core.hooksPath"])
        .expect("hooksPath should still exist");
    assert_eq!(
        hooks_path.trim(),
        user_hooks_dir.to_string_lossy().as_ref(),
        "core.hooksPath should remain at the user's original value"
    );
}

#[test]
#[serial]
fn externally_managed_preserves_original_hooks_path_for_remove() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    // Set up a pre-existing hooks path that will be overwritten by managed mode.
    let user_hooks_dir = git_common_dir(&repo).join("custom-hooks");
    fs::create_dir_all(&user_hooks_dir).expect("failed to create custom hooks dir");
    repo.git(&[
        "config",
        "--local",
        "core.hooksPath",
        user_hooks_dir.to_string_lossy().as_ref(),
    ])
    .expect("setting preexisting local hooksPath should succeed");

    // First ensure in managed mode so the original hooksPath is saved in state.
    repo.git_ai_with_env(
        &["git-hooks", "ensure"],
        &[
            ("GIT_AI_GIT_HOOKS_ENABLED", "true"),
            ("GIT_AI_GIT_HOOKS_EXTERNALLY_MANAGED", "false"),
        ],
    )
    .expect("git-hooks ensure should succeed");

    let managed_hooks_dir = git_hooks_ai_dir(&repo).join("hooks");
    let hooks_path_after_first_ensure = repo
        .git(&["config", "--local", "--get", "core.hooksPath"])
        .expect("hooksPath should exist after ensure");
    // Canonicalize to handle macOS /var -> /private/var symlink.
    let canon_actual = fs::canonicalize(hooks_path_after_first_ensure.trim())
        .expect("canonicalize actual hooks path");
    let canon_expected =
        fs::canonicalize(&managed_hooks_dir).expect("canonicalize managed hooks dir");
    assert_eq!(
        canon_actual, canon_expected,
        "first ensure should set core.hooksPath to managed hooks dir"
    );

    // Switch to externally managed mode and ensure again; this should not drop the
    // previously saved original hooksPath from state.
    repo.git_ai_with_env(
        &["git-hooks", "ensure"],
        &[
            ("GIT_AI_GIT_HOOKS_ENABLED", "true"),
            ("GIT_AI_GIT_HOOKS_EXTERNALLY_MANAGED", "true"),
        ],
    )
    .expect("git-hooks ensure with externally_managed should succeed");

    repo.git_ai(&["git-hooks", "remove"])
        .expect("git-hooks remove should succeed");

    let hooks_path_after_remove = repo
        .git(&["config", "--local", "--get", "core.hooksPath"])
        .expect("hooksPath should be restored after remove");
    // Canonicalize to handle macOS /var -> /private/var symlink.
    let canon_after_remove =
        fs::canonicalize(hooks_path_after_remove.trim()).expect("canonicalize restored hooks path");
    let canon_user_hooks = fs::canonicalize(&user_hooks_dir).expect("canonicalize user hooks dir");
    assert_eq!(
        canon_after_remove, canon_user_hooks,
        "remove should restore the user's original core.hooksPath"
    );
}

#[test]
#[serial]
fn externally_managed_without_hooks_enabled_has_no_effect() {
    // When hooks_externally_managed is true but hooks_enabled is false,
    // the externally_managed flag should have no effect
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    // Remove hooks to start clean
    let _ = repo.git_ai(&["git-hooks", "remove"]);

    let managed_hooks_dir = git_hooks_ai_dir(&repo).join("hooks");

    fs::write(
        repo.path().join("no-effect-test.txt"),
        "testing externally_managed without hooks_enabled\n",
    )
    .expect("failed to write test file");
    repo.git(&["add", "no-effect-test.txt"])
        .expect("staging should succeed");

    // hooks_externally_managed=true but hooks_enabled=false
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "no-effect-test.txt"],
        &[
            ("GIT_AI_GIT_HOOKS_ENABLED", "false"),
            ("GIT_AI_GIT_HOOKS_EXTERNALLY_MANAGED", "true"),
        ],
    )
    .expect("checkpoint should succeed");

    // Hooks should NOT be ensured because hooks_enabled is false
    assert!(
        !managed_hooks_dir.exists() || managed_hooks_dir.symlink_metadata().is_err(),
        "managed hooks should NOT exist when hooks_enabled is false regardless of externally_managed"
    );
}

// =============================================================================
// CLI config set/get/unset for the new feature flags
// =============================================================================

#[test]
#[serial]
fn cli_config_set_and_get_hooks_enabled() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    // Set hooks_enabled via CLI config
    repo.git_ai(&["config", "set", "feature_flags.git_hooks_enabled", "true"])
        .expect("config set hooks_enabled should succeed");

    // Get the value back (config get syntax is: git-ai config <key>)
    let result = repo
        .git_ai(&["config", "feature_flags.git_hooks_enabled"])
        .expect("config get hooks_enabled should succeed");
    assert!(
        result.contains("true"),
        "config get should return true for hooks_enabled, got: {}",
        result
    );

    // Set to false
    repo.git_ai(&["config", "set", "feature_flags.git_hooks_enabled", "false"])
        .expect("config set hooks_enabled to false should succeed");

    let result = repo
        .git_ai(&["config", "feature_flags.git_hooks_enabled"])
        .expect("config get hooks_enabled should succeed");
    assert!(
        result.contains("false"),
        "config get should return false for hooks_enabled, got: {}",
        result
    );
}

#[test]
#[serial]
fn cli_config_set_and_get_hooks_externally_managed() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    // Set hooks_externally_managed via CLI config
    repo.git_ai(&[
        "config",
        "set",
        "feature_flags.git_hooks_externally_managed",
        "true",
    ])
    .expect("config set hooks_externally_managed should succeed");

    // Get the value back (config get syntax is: git-ai config <key>)
    let result = repo
        .git_ai(&["config", "feature_flags.git_hooks_externally_managed"])
        .expect("config get hooks_externally_managed should succeed");
    assert!(
        result.contains("true"),
        "config get should return true for hooks_externally_managed, got: {}",
        result
    );
}

#[test]
#[serial]
fn cli_config_unset_hooks_enabled() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    // Set then unset
    repo.git_ai(&["config", "set", "feature_flags.git_hooks_enabled", "true"])
        .expect("config set should succeed");
    repo.git_ai(&["config", "unset", "feature_flags.git_hooks_enabled"])
        .expect("config unset hooks_enabled should succeed");

    // After unset, getting the key should either error or show the default (false)
    let result = repo.git_ai(&["config", "feature_flags.git_hooks_enabled"]);
    // It's acceptable for this to either return the default or to report the key isn't set
    match result {
        Ok(output) => {
            // If we get output, it should reflect the default (false)
            assert!(
                output.contains("false") || output.contains("null") || output.trim().is_empty(),
                "after unset, hooks_enabled should be default/null, got: {}",
                output
            );
        }
        Err(_) => {
            // An error (key not found) is also acceptable after unset
        }
    }
}

#[test]
#[serial]
fn cli_config_unset_hooks_externally_managed() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    // Set then unset
    repo.git_ai(&[
        "config",
        "set",
        "feature_flags.git_hooks_externally_managed",
        "true",
    ])
    .expect("config set should succeed");
    repo.git_ai(&[
        "config",
        "unset",
        "feature_flags.git_hooks_externally_managed",
    ])
    .expect("config unset hooks_externally_managed should succeed");

    let result = repo.git_ai(&["config", "feature_flags.git_hooks_externally_managed"]);
    match result {
        Ok(output) => {
            assert!(
                output.contains("false") || output.contains("null") || output.trim().is_empty(),
                "after unset, hooks_externally_managed should be default/null, got: {}",
                output
            );
        }
        Err(_) => {
            // An error (key not found) is also acceptable after unset
        }
    }
}

#[test]
#[serial]
fn cli_config_set_succeeds_with_utf8_bom_prefixed_config_file() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    let test_home = repo.test_home_path();
    let config_dir = test_home.join(".git-ai");
    let config_path = config_dir.join("config.json");
    fs::create_dir_all(&config_dir).expect("failed to create config directory");

    let mut config_bytes = vec![0xEF, 0xBB, 0xBF];
    config_bytes.extend_from_slice(br#"{"git_path":"C:\\Program Files\\Git\\cmd\\git.exe"}"#);
    fs::write(&config_path, config_bytes).expect("failed to write BOM-prefixed config");

    let home = test_home.to_string_lossy().to_string();
    repo.git_ai_with_env(
        &["config", "set", "feature_flags.git_hooks_enabled", "true"],
        &[("HOME", &home), ("USERPROFILE", &home)],
    )
    .expect("config set should succeed for BOM-prefixed config");

    let result = repo
        .git_ai_with_env(
            &["config", "feature_flags.git_hooks_enabled"],
            &[("HOME", &home), ("USERPROFILE", &home)],
        )
        .expect("config get should succeed for BOM-prefixed config");

    assert!(
        result.contains("true"),
        "config get should return true after set, got: {}",
        result
    );
}
