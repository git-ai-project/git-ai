mod repos;

use repos::test_repo::TestRepo;
use serial_test::serial;
use std::fs;

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

#[test]
#[serial]
fn hook_mode_runs_without_wrapper() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    fs::write(
        repo.path().join("hooks-mode.txt"),
        "hello from hooks mode\n",
    )
    .expect("failed to write test file");
    repo.git(&["add", "hooks-mode.txt"])
        .expect("staging should succeed");

    repo.git_ai(&["checkpoint", "mock_ai", "hooks-mode.txt"])
        .expect("checkpoint should succeed");

    let commit = repo
        .commit("commit via hooks mode")
        .expect("commit should succeed in hooks mode");

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "hooks mode should still produce authorship data"
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn wrapper_and_hooks_do_not_double_run_managed_logic() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "both");

    let repo = TestRepo::new();

    let user_hooks_dir = repo.path().join(".git").join("custom-hooks");
    fs::create_dir_all(&user_hooks_dir).expect("failed to create user hooks dir");

    let marker_path = repo.path().join(".git").join("hook-marker.txt");
    let pre_commit_path = user_hooks_dir.join("pre-commit");
    fs::write(
        &pre_commit_path,
        format!(
            "#!/bin/sh\necho forwarded >> '{}'\n",
            marker_path.to_string_lossy()
        ),
    )
    .expect("failed to write forwarded pre-commit hook");

    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&pre_commit_path)
        .expect("failed to stat pre-commit hook")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&pre_commit_path, perms).expect("failed to set hook executable bit");

    let repo_state_path = repo
        .path()
        .join(".git")
        .join("ai")
        .join("git_hooks_state.json");
    fs::create_dir_all(
        repo_state_path
            .parent()
            .expect("repo state should have parent directory"),
    )
    .expect("failed to create repo state directory");
    fs::write(
        &repo_state_path,
        format!(
            "{{\n  \"previous_hooks_path\": \"{}\"\n}}\n",
            user_hooks_dir.to_string_lossy().replace('\\', "\\\\")
        ),
    )
    .expect("failed to write repo hook state");

    fs::write(repo.path().join("both-mode.txt"), "hello from both mode\n")
        .expect("failed to write test file");
    repo.git(&["add", "both-mode.txt"])
        .expect("staging should succeed");

    repo.git_ai(&["checkpoint", "mock_ai", "both-mode.txt"])
        .expect("checkpoint should succeed");

    repo.commit("commit with wrapper+hooks")
        .expect("commit should succeed");

    let marker_content = fs::read_to_string(&marker_path).expect("marker hook should run");
    let invocation_count = marker_content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();

    assert_eq!(
        invocation_count, 1,
        "forwarded pre-commit hook should run exactly once"
    );

    let rewrite_log = fs::read_to_string(repo.path().join(".git").join("ai").join("rewrite_log"))
        .expect("rewrite log should exist");
    let commit_events = rewrite_log
        .lines()
        .filter(|line| line.contains("\"commit\""))
        .count();

    assert_eq!(
        commit_events, 1,
        "wrapper+hooks mode should not duplicate commit rewrite-log events"
    );
}
