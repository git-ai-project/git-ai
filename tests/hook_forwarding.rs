mod repos;

use repos::test_repo::TestRepo;
use serial_test::serial;
use std::fs;
use std::path::{Path, PathBuf};

struct EnvVarGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, old }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(old) = &self.old {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .expect("failed to stat file")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("failed to set executable bit");
}

fn managed_hooks_dir(repo: &TestRepo) -> PathBuf {
    repo.path().join(".git").join("ai").join("hooks")
}

fn hook_state_path(repo: &TestRepo) -> PathBuf {
    repo.path()
        .join(".git")
        .join("ai")
        .join("git_hooks_state.json")
}

fn configure_forward_target(repo: &TestRepo, forward_dir: &Path) {
    repo.git(&[
        "config",
        "--local",
        "core.hooksPath",
        forward_dir.to_string_lossy().as_ref(),
    ])
    .expect("setting core.hooksPath should succeed");

    repo.git_ai(&["git-hooks", "ensure"])
        .expect("git-hooks ensure should succeed");
}

fn prepare_file(repo: &TestRepo, filename: &str) {
    fs::write(repo.path().join(filename), "hello\n").expect("failed to write file");
    repo.git(&["add", filename])
        .expect("git add should succeed");
}

#[cfg(unix)]
fn commit_msg_marker_script(marker_path: &Path) -> String {
    format!(
        "#!/bin/sh\necho commit-msg-fired >> '{}'\n",
        marker_path.to_string_lossy()
    )
}

// ---------------------------------------------------------------------------
// 1. Hooks mode forwards non-managed commit-msg hook
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_forwards_non_managed_commit_msg_hook() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    let forward_dir = repo.path().join(".husky");
    fs::create_dir_all(&forward_dir).expect("failed to create forward dir");

    let marker_path = repo.path().join(".git").join("commit-msg-marker.txt");
    let commit_msg_hook = forward_dir.join("commit-msg");
    fs::write(&commit_msg_hook, commit_msg_marker_script(&marker_path))
        .expect("failed to write commit-msg hook");
    set_executable(&commit_msg_hook);

    configure_forward_target(&repo, &forward_dir);

    assert!(
        managed_hooks_dir(&repo)
            .join("commit-msg")
            .symlink_metadata()
            .is_ok(),
        "commit-msg should be provisioned when it exists in the forward target"
    );

    prepare_file(&repo, "forwarded.txt");

    repo.git(&["commit", "-m", "forwarded commit-msg hook"])
        .expect("commit should succeed");

    let marker = fs::read_to_string(&marker_path).expect("marker should exist");
    let count = marker
        .lines()
        .filter(|line| line.trim() == "commit-msg-fired")
        .count();
    assert_eq!(count, 1, "commit-msg should fire exactly once");
}

// ---------------------------------------------------------------------------
// 2. commit-msg hook receives the message file argument
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_commit_msg_receives_message_file_arg() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    let forward_dir = repo.path().join(".git").join("arg-hooks");
    fs::create_dir_all(&forward_dir).expect("failed to create forward dir");

    let marker_path = repo.path().join(".git").join("arg-marker.txt");
    let commit_msg_hook = forward_dir.join("commit-msg");
    fs::write(
        &commit_msg_hook,
        format!(
            "#!/bin/sh\nfirst=\"$(head -n 1 \"$1\")\"\necho \"msg:${{first}}\" >> '{}'\n",
            marker_path.to_string_lossy()
        ),
    )
    .expect("failed to write commit-msg hook");
    set_executable(&commit_msg_hook);

    configure_forward_target(&repo, &forward_dir);

    prepare_file(&repo, "arg.txt");

    repo.git(&["commit", "-m", "verify arg passing"])
        .expect("commit should succeed");

    let marker = fs::read_to_string(&marker_path).expect("marker should exist");
    assert!(
        marker.contains("msg:verify arg passing"),
        "expected commit-msg hook to read the commit message; got: {}",
        marker
    );
}

// ---------------------------------------------------------------------------
// 3. Forwarded commit-msg failure blocks the commit
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_commit_msg_failure_propagates() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    let forward_dir = repo.path().join(".git").join("failing-hooks");
    fs::create_dir_all(&forward_dir).expect("failed to create forward dir");

    let commit_msg_hook = forward_dir.join("commit-msg");
    fs::write(&commit_msg_hook, "#!/bin/sh\nexit 2\n").expect("failed to write hook");
    set_executable(&commit_msg_hook);

    configure_forward_target(&repo, &forward_dir);

    prepare_file(&repo, "fail.txt");

    let result = repo.git(&["commit", "-m", "should fail"]);
    assert!(
        result.is_err(),
        "commit should fail when forwarded commit-msg exits non-zero"
    );
}

// ---------------------------------------------------------------------------
// 4. Non-managed hooks are not provisioned when the forward dir lacks them
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_non_managed_hooks_not_provisioned_without_original() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    let forward_dir = repo.path().join(".git").join("empty-forward");
    fs::create_dir_all(&forward_dir).expect("failed to create forward dir");

    configure_forward_target(&repo, &forward_dir);

    let managed_dir = managed_hooks_dir(&repo);
    assert!(
        !managed_dir.join("commit-msg").exists()
            && managed_dir.join("commit-msg").symlink_metadata().is_err(),
        "commit-msg should not be provisioned when it does not exist in the forward target"
    );

    prepare_file(&repo, "no-hook.txt");
    repo.git(&["commit", "-m", "no commit-msg hook"])
        .expect("commit should succeed");
}

// ---------------------------------------------------------------------------
// 5. ensure picks up a newly added non-managed hook
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_ensure_picks_up_new_hook_in_forward_dir() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    let forward_dir = repo.path().join(".git").join("dynamic-forward");
    fs::create_dir_all(&forward_dir).expect("failed to create forward dir");

    configure_forward_target(&repo, &forward_dir);

    let managed_dir = managed_hooks_dir(&repo);
    assert!(
        managed_dir.join("commit-msg").symlink_metadata().is_err(),
        "commit-msg should not be provisioned before it exists in the forward dir"
    );

    let commit_msg_hook = forward_dir.join("commit-msg");
    fs::write(&commit_msg_hook, "#!/bin/sh\nexit 0\n").expect("failed to write hook");
    set_executable(&commit_msg_hook);

    repo.git_ai(&["git-hooks", "ensure"])
        .expect("re-ensure should succeed");

    assert!(
        managed_dir.join("commit-msg").symlink_metadata().is_ok(),
        "commit-msg should be provisioned after being added to forward dir"
    );
}

// ---------------------------------------------------------------------------
// 6. ensure removes stale non-managed hook symlink after deletion
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_ensure_removes_stale_symlink() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    let forward_dir = repo.path().join(".git").join("stale-forward");
    fs::create_dir_all(&forward_dir).expect("failed to create forward dir");

    let commit_msg_hook = forward_dir.join("commit-msg");
    fs::write(&commit_msg_hook, "#!/bin/sh\nexit 0\n").expect("failed to write hook");
    set_executable(&commit_msg_hook);

    configure_forward_target(&repo, &forward_dir);

    let managed_dir = managed_hooks_dir(&repo);
    assert!(
        managed_dir.join("commit-msg").symlink_metadata().is_ok(),
        "commit-msg should be provisioned initially"
    );

    fs::remove_file(&commit_msg_hook).expect("failed to remove original hook");

    repo.git_ai(&["git-hooks", "ensure"])
        .expect("re-ensure should succeed");

    assert!(
        managed_dir.join("commit-msg").symlink_metadata().is_err(),
        "stale commit-msg symlink should be removed after original is deleted"
    );
}

// ---------------------------------------------------------------------------
// 7. Husky-style hooks using dirname "$0" work via forwarding
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_husky_style_dirname_resolution() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    let husky_dir = repo.path().join(".husky");
    let internal = husky_dir.join("_");
    fs::create_dir_all(&internal).expect("failed to create .husky/_");

    fs::write(internal.join("husky.sh"), "#!/bin/sh\n").expect("failed to write husky.sh");
    set_executable(&internal.join("husky.sh"));

    let marker_path = repo.path().join(".git").join("husky-marker.txt");
    let commit_msg_hook = husky_dir.join("commit-msg");
    fs::write(
        &commit_msg_hook,
        format!(
            "#!/bin/sh\nhook_dir=\"$(dirname \"$0\")\"\nif [ -f \"$hook_dir/_/husky.sh\" ]; then\n  echo husky-ok >> '{}'\nelse\n  echo husky-broken:$hook_dir >> '{}'\nfi\n",
            marker_path.to_string_lossy(),
            marker_path.to_string_lossy()
        ),
    )
    .expect("failed to write husky commit-msg hook");
    set_executable(&commit_msg_hook);

    configure_forward_target(&repo, &husky_dir);

    prepare_file(&repo, "husky.txt");

    repo.git(&["commit", "-m", "husky test"])
        .expect("commit should succeed");

    let marker = fs::read_to_string(&marker_path).expect("marker should exist");
    assert!(
        marker.contains("husky-ok"),
        "expected husky dirname resolution to succeed; got: {}",
        marker
    );
    assert!(
        !marker.contains("husky-broken"),
        "expected husky dirname resolution to not be broken; got: {}",
        marker
    );
}

// ---------------------------------------------------------------------------
// 8. Directory entry in forward dir is ignored (not treated as hook)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_directory_in_forward_dir_ignored() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    let forward_dir = repo.path().join(".git").join("dir-forward");
    fs::create_dir_all(&forward_dir).expect("failed to create forward dir");
    fs::create_dir_all(forward_dir.join("commit-msg")).expect("failed to create hook directory");

    configure_forward_target(&repo, &forward_dir);

    assert!(
        managed_hooks_dir(&repo)
            .join("commit-msg")
            .symlink_metadata()
            .is_err(),
        "directory named commit-msg should not be provisioned"
    );
}

// ---------------------------------------------------------------------------
// 9. Non-executable hook exists but is skipped during forwarding
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_non_executable_forwarded_hook_skipped() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    let forward_dir = repo.path().join(".git").join("noexec-forward");
    fs::create_dir_all(&forward_dir).expect("failed to create forward dir");

    let marker_path = repo.path().join(".git").join("noexec-marker.txt");
    let commit_msg_hook = forward_dir.join("commit-msg");
    fs::write(
        &commit_msg_hook,
        format!(
            "#!/bin/sh\necho ran >> '{}'\nexit 1\n",
            marker_path.to_string_lossy()
        ),
    )
    .expect("failed to write hook");

    configure_forward_target(&repo, &forward_dir);

    prepare_file(&repo, "noexec.txt");

    repo.git(&["commit", "-m", "non-exec hook should be skipped"])
        .expect("commit should succeed");

    assert!(
        fs::read_to_string(&marker_path).is_err(),
        "marker should not exist because non-executable hook should not run"
    );
}

// ---------------------------------------------------------------------------
// 10. Both mode: non-managed hook executes exactly once
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn both_mode_non_managed_hook_runs_exactly_once() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "both");

    let repo = TestRepo::new();

    let forward_dir = repo.path().join(".git").join("both-forward");
    fs::create_dir_all(&forward_dir).expect("failed to create forward dir");

    let marker_path = repo.path().join(".git").join("both-marker.txt");
    let commit_msg_hook = forward_dir.join("commit-msg");
    fs::write(&commit_msg_hook, commit_msg_marker_script(&marker_path))
        .expect("failed to write commit-msg hook");
    set_executable(&commit_msg_hook);

    configure_forward_target(&repo, &forward_dir);

    prepare_file(&repo, "both.txt");

    repo.git(&["commit", "-m", "both mode commit"])
        .expect("commit should succeed");

    let marker = fs::read_to_string(&marker_path).expect("marker should exist");
    let count = marker
        .lines()
        .filter(|line| line.trim() == "commit-msg-fired")
        .count();
    assert_eq!(
        count, 1,
        "commit-msg should run once in both mode, ran {} times",
        count
    );
}

// ---------------------------------------------------------------------------
// 11. Non-managed hook symlinks point to same git-ai binary as managed hooks
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_non_managed_symlinks_point_to_git_ai_binary() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    let forward_dir = repo.path().join(".git").join("binary-target");
    fs::create_dir_all(&forward_dir).expect("failed to create forward dir");

    let commit_msg_hook = forward_dir.join("commit-msg");
    fs::write(&commit_msg_hook, "#!/bin/sh\nexit 0\n").expect("failed to write hook");
    set_executable(&commit_msg_hook);

    configure_forward_target(&repo, &forward_dir);

    let managed_dir = managed_hooks_dir(&repo);
    let non_managed_link = managed_dir.join("commit-msg");
    let managed_link = managed_dir.join("pre-commit");

    let non_managed_target =
        fs::read_link(&non_managed_link).expect("should read non-managed symlink target");
    let managed_target = fs::read_link(&managed_link).expect("should read managed symlink target");

    let non_managed_canon =
        fs::canonicalize(&non_managed_target).unwrap_or_else(|_| non_managed_target.clone());
    let managed_canon =
        fs::canonicalize(&managed_target).unwrap_or_else(|_| managed_target.clone());

    assert_eq!(
        non_managed_canon, managed_canon,
        "non-managed hook should symlink to git-ai binary (same as managed hooks)"
    );
}

// ---------------------------------------------------------------------------
// 12. State file records forward target after ensure
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_state_file_records_forward_target() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    let forward_dir = repo.path().join(".git").join("state-forward");
    fs::create_dir_all(&forward_dir).expect("failed to create forward dir");

    configure_forward_target(&repo, &forward_dir);

    let state_raw = fs::read_to_string(hook_state_path(&repo)).expect("state should exist");
    let state: serde_json::Value = serde_json::from_str(&state_raw).expect("valid JSON");

    assert_eq!(state["forward_mode"].as_str(), Some("repo_local"));
    assert_eq!(
        state["forward_hooks_path"].as_str().map(|s| s.trim()),
        Some(forward_dir.to_string_lossy().trim())
    );
}

// ---------------------------------------------------------------------------
// 13. Managed hooks are installed in hooks mode
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_managed_hooks_always_installed() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();
    let managed_dir = managed_hooks_dir(&repo);

    for hook_name in [
        "pre-commit",
        "prepare-commit-msg",
        "post-commit",
        "pre-rebase",
        "post-checkout",
        "post-merge",
        "pre-push",
        "post-rewrite",
        "reference-transaction",
    ] {
        let hook_path = managed_dir.join(hook_name);
        assert!(
            hook_path.exists() || hook_path.symlink_metadata().is_ok(),
            "managed hook {} should be installed",
            hook_name
        );
    }
}

// ---------------------------------------------------------------------------
// 14. Managed attribution still works with forwarding enabled
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn hooks_mode_managed_hooks_still_produce_authorship_with_forwarding() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    let forward_dir = repo.path().join(".git").join("authorship-forward");
    fs::create_dir_all(&forward_dir).expect("failed to create forward dir");

    let commit_msg_hook = forward_dir.join("commit-msg");
    fs::write(&commit_msg_hook, "#!/bin/sh\nexit 0\n").expect("failed to write hook");
    set_executable(&commit_msg_hook);

    configure_forward_target(&repo, &forward_dir);

    prepare_file(&repo, "authorship.txt");

    repo.git_ai(&["checkpoint", "mock_ai", "authorship.txt"])
        .expect("checkpoint should succeed");

    let commit = repo
        .commit("authorship with forwarding")
        .expect("commit should succeed");

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "expected authorship attestations to be present"
    );
}

// ---------------------------------------------------------------------------
// 15. Wrapper mode does not set up repo-local hook directory
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn wrapper_mode_does_not_install_hook_symlinks() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");

    let repo = TestRepo::new();

    assert!(
        !managed_hooks_dir(&repo).exists(),
        "managed hooks dir should not exist in wrapper-only mode"
    );
}
