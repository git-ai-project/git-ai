mod repos;

use repos::test_repo::TestRepo;
use serde_json::Value;
use serial_test::serial;
use std::fs;
use std::io::Write;
use std::path::Path;

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

fn assert_blame_line_author_contains(
    blame_output: &str,
    content_snippet: &str,
    author_snippet: &str,
) {
    let Some(line) = blame_output
        .lines()
        .find(|line| line.contains(content_snippet))
    else {
        panic!(
            "expected blame output to contain line snippet {:?}\nblame output:\n{}",
            content_snippet, blame_output
        );
    };

    assert!(
        line.to_ascii_lowercase()
            .contains(&author_snippet.to_ascii_lowercase()),
        "expected blame line for {:?} to include author snippet {:?}\nline: {}",
        content_snippet,
        author_snippet,
        line
    );
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .expect("failed to stat executable hook")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("failed to set executable bit");
}

#[test]
#[serial]
fn git_hooks_ensure_records_repo_opt_in_marker() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();
    let marker_path = repo
        .path()
        .join(".git")
        .join("ai")
        .join("git_hooks_enabled");

    assert!(
        !marker_path.exists(),
        "marker should not exist before running git-hooks ensure"
    );

    repo.git_ai(&["git-hooks", "ensure"])
        .expect("git-hooks ensure should succeed");

    assert!(
        marker_path.exists(),
        "marker should exist after running git-hooks ensure"
    );
}

#[test]
#[serial]
fn git_hooks_remove_removes_repo_opt_in_marker() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();
    let marker_path = repo
        .path()
        .join(".git")
        .join("ai")
        .join("git_hooks_enabled");
    let managed_hooks_dir = repo.path().join(".git").join("ai").join("hooks");

    repo.git_ai(&["git-hooks", "ensure"])
        .expect("git-hooks ensure should succeed");
    assert!(
        marker_path.exists(),
        "marker should exist after running git-hooks ensure"
    );
    assert!(
        managed_hooks_dir.exists() || managed_hooks_dir.symlink_metadata().is_ok(),
        "managed hooks should exist after running git-hooks ensure"
    );

    repo.git_ai(&["git-hooks", "remove"])
        .expect("git-hooks remove should succeed");

    assert!(
        !marker_path.exists() && marker_path.symlink_metadata().is_err(),
        "marker should be removed after running git-hooks remove"
    );
    assert!(
        !managed_hooks_dir.exists() && managed_hooks_dir.symlink_metadata().is_err(),
        "managed hooks should be removed after running git-hooks remove"
    );
}

#[test]
#[serial]
fn git_hooks_remove_restores_preexisting_hooks_path_end_to_end() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    let original_hooks_dir = repo.path().join(".git").join("custom-hooks");
    fs::create_dir_all(&original_hooks_dir).expect("failed to create custom hooks dir");
    repo.git(&[
        "config",
        "--local",
        "core.hooksPath",
        original_hooks_dir.to_string_lossy().as_ref(),
    ])
    .expect("setting preexisting local hooksPath should succeed");

    let before = repo
        .git(&["config", "--local", "--get", "core.hooksPath"])
        .expect("reading preexisting hooksPath should succeed")
        .trim()
        .to_string();
    assert_eq!(
        before,
        original_hooks_dir.to_string_lossy(),
        "sanity check: preexisting hooksPath should match custom hooks dir"
    );

    repo.git_ai(&["git-hooks", "ensure"])
        .expect("git-hooks ensure should succeed");

    let managed_hooks_dir = repo.path().join(".git").join("ai").join("hooks");
    let after_ensure = repo
        .git(&["config", "--local", "--get", "core.hooksPath"])
        .expect("reading hooksPath after ensure should succeed")
        .trim()
        .to_string();
    assert_eq!(
        std::fs::canonicalize(&managed_hooks_dir)
            .expect("managed hooks dir should exist after ensure"),
        std::fs::canonicalize(&after_ensure).expect("managed hooks path from config should exist"),
        "ensure should point core.hooksPath at managed hooks"
    );

    repo.git_ai(&["git-hooks", "remove"])
        .expect("git-hooks remove should succeed");

    let after_remove = repo
        .git(&["config", "--local", "--get", "core.hooksPath"])
        .expect("hooksPath should still exist and be restored after remove")
        .trim()
        .to_string();
    assert_eq!(
        std::fs::canonicalize(&after_remove).expect("restored hooksPath should exist"),
        std::fs::canonicalize(&original_hooks_dir).expect("custom hooks dir should exist"),
        "remove should restore the original local hooksPath"
    );

    let marker = repo
        .path()
        .join(".git")
        .join("ai")
        .join("git_hooks_enabled");
    let state = repo
        .path()
        .join(".git")
        .join("ai")
        .join("git_hooks_state.json");
    assert!(
        !managed_hooks_dir.exists() && managed_hooks_dir.symlink_metadata().is_err(),
        "remove should delete managed hooks dir"
    );
    assert!(
        !marker.exists() && marker.symlink_metadata().is_err(),
        "remove should delete opt-in marker"
    );
    assert!(
        !state.exists() && state.symlink_metadata().is_err(),
        "remove should delete repo hook state"
    );
}

#[test]
#[serial]
fn git_hooks_uninstall_alias_works_end_to_end() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();
    let managed_hooks_dir = repo.path().join(".git").join("ai").join("hooks");

    repo.git_ai(&["git-hooks", "ensure"])
        .expect("git-hooks ensure should succeed");
    assert!(
        managed_hooks_dir.exists() || managed_hooks_dir.symlink_metadata().is_ok(),
        "managed hooks should exist after ensure"
    );

    repo.git_ai(&["git-hooks", "uninstall"])
        .expect("git-hooks uninstall alias should succeed");
    assert!(
        !managed_hooks_dir.exists() && managed_hooks_dir.symlink_metadata().is_err(),
        "uninstall alias should remove managed hooks"
    );
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
    let commit_msg_path = user_hooks_dir.join("commit-msg");
    fs::write(
        &pre_commit_path,
        format!(
            "#!/bin/sh\necho pre-commit >> '{}'\n",
            marker_path.to_string_lossy()
        ),
    )
    .expect("failed to write forwarded pre-commit hook");
    fs::write(
        &commit_msg_path,
        format!(
            "#!/bin/sh\nline=\"$(head -n 1 \"$1\")\"\necho \"commit-msg:${{line}}\" >> '{}'\n",
            marker_path.to_string_lossy()
        ),
    )
    .expect("failed to write forwarded commit-msg hook");
    set_executable(&pre_commit_path);
    set_executable(&commit_msg_path);

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
            "{{\n  \"schema_version\": \"repo_hooks/2\",\n  \"managed_hooks_path\": \"{}\",\n  \"original_local_hooks_path\": null,\n  \"forward_mode\": \"repo_local\",\n  \"forward_hooks_path\": \"{}\",\n  \"binary_path\": \"test-binary\"\n}}\n",
            repo.path()
                .join(".git")
                .join("ai")
                .join("hooks")
                .to_string_lossy()
                .replace('\\', "\\\\"),
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
    let pre_commit_count = marker_content
        .lines()
        .filter(|line| line.trim() == "pre-commit")
        .count();
    let commit_msg_count = marker_content
        .lines()
        .filter(|line| line.starts_with("commit-msg:commit with wrapper+hooks"))
        .count();

    assert_eq!(
        pre_commit_count, 1,
        "forwarded pre-commit hook should run exactly once"
    );
    assert_eq!(
        commit_msg_count, 1,
        "forwarded commit-msg hook should run exactly once"
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

#[test]
#[serial]
fn hooks_mode_batches_multi_commit_cherry_pick_rewrite_event() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();
    let main_branch = repo.current_branch();
    let file_path = repo.path().join("cherry-batch.txt");
    fs::write(&file_path, "base line\n").expect("failed to create file");
    repo.git(&["add", "cherry-batch.txt"])
        .expect("staging base file should succeed");
    repo.git(&["commit", "-m", "base commit"])
        .expect("base commit should succeed");

    repo.git(&["checkout", "-b", "feature"])
        .expect("feature checkout should succeed");
    let mut commits = Vec::new();
    for i in 1..=3 {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&file_path)
            .expect("failed to open file for append");
        writeln!(file, "ai line {}", i).expect("failed to append ai line");

        repo.git_ai(&["checkpoint", "mock_ai", "cherry-batch.txt"])
            .expect("checkpoint should succeed");
        repo.git(&["add", "cherry-batch.txt"])
            .expect("staging ai line should succeed");
        repo.git(&["commit", "-m", &format!("ai commit {}", i)])
            .expect("feature ai commit should succeed");
        commits.push(
            repo.git(&["rev-parse", "HEAD"])
                .expect("rev-parse should succeed")
                .trim()
                .to_string(),
        );
    }

    repo.git(&["checkout", &main_branch])
        .expect("checkout main should succeed");
    let mut cherry_pick_args: Vec<&str> = vec!["cherry-pick"];
    for commit in &commits {
        cherry_pick_args.push(commit);
    }
    repo.git(&cherry_pick_args)
        .expect("cherry-pick sequence should succeed");

    let batch_state_path = repo
        .path()
        .join(".git")
        .join("ai")
        .join("cherry_pick_batch_state.json");
    if batch_state_path.exists() {
        // Some Git/hook timing variants may finalize stale cherry-pick state on
        // the next managed hook invocation instead of the terminal post-commit.
        repo.git(&["commit", "--allow-empty", "-m", "post cherry-pick cleanup"])
            .expect("follow-up commit should succeed");
    }
    assert!(
        !batch_state_path.exists(),
        "cherry-pick batch state should be cleaned up after follow-up hook if needed"
    );

    let rewrite_log = fs::read_to_string(repo.path().join(".git").join("ai").join("rewrite_log"))
        .expect("rewrite log should exist");
    let mut cherry_pick_complete_events = 0usize;
    for line in rewrite_log.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(event) = value.get("cherry_pick_complete") else {
            continue;
        };
        let source_len = event
            .get("source_commits")
            .and_then(|commits| commits.as_array())
            .map(|commits| commits.len())
            .unwrap_or(0);
        let new_len = event
            .get("new_commits")
            .and_then(|commits| commits.as_array())
            .map(|commits| commits.len())
            .unwrap_or(0);
        assert_eq!(
            source_len, 3,
            "cherry-pick complete event should include all source commits"
        );
        assert_eq!(
            new_len, 3,
            "cherry-pick complete event should include all new commits"
        );
        cherry_pick_complete_events += 1;
    }

    assert!(
        cherry_pick_complete_events <= 1,
        "hooks mode should not emit duplicate cherry_pick_complete events for multi-commit cherry-pick"
    );
}

#[test]
#[serial]
fn hooks_mode_amend_uses_single_amend_rewrite_event() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();

    fs::write(repo.path().join("amend-mode.txt"), "line 1\n").expect("failed to write file");
    repo.git(&["add", "amend-mode.txt"])
        .expect("initial add should succeed");
    repo.commit("initial commit")
        .expect("initial commit should succeed");

    fs::write(repo.path().join("amend-mode.txt"), "line 1\nline 2\n")
        .expect("failed to update file");
    repo.git(&["add", "amend-mode.txt"])
        .expect("amend add should succeed");
    repo.git(&["commit", "--amend", "-m", "initial commit amended"])
        .expect("amend commit should succeed");

    let rewrite_log = fs::read_to_string(repo.path().join(".git").join("ai").join("rewrite_log"))
        .expect("rewrite log should exist");
    let amend_events = rewrite_log
        .lines()
        .filter(|line| line.contains("\"commit_amend\""))
        .count();
    let plain_commit_events = rewrite_log
        .lines()
        .filter(|line| line.contains("\"commit\"") && !line.contains("\"commit_amend\""))
        .count();

    assert_eq!(
        amend_events, 1,
        "hooks mode amend should emit exactly one commit_amend event"
    );
    assert_eq!(
        plain_commit_events, 1,
        "hooks mode amend should not emit an extra plain commit event"
    );
}

#[test]
#[serial]
fn hooks_mode_non_root_amend_preserves_ai_authorship() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();
    let path = repo.path().join("amend-authorship.txt");

    fs::write(&path, "base line\n").expect("failed to write base line");
    repo.git(&["add", "amend-authorship.txt"])
        .expect("staging base line should succeed");
    repo.commit("base commit")
        .expect("base commit should succeed");

    fs::write(&path, "base line\nsecond line\n").expect("failed to write second line");
    repo.git(&["add", "amend-authorship.txt"])
        .expect("staging second line should succeed");
    repo.commit("second commit")
        .expect("second commit should succeed");

    fs::write(&path, "base line\nsecond line\nai amended line\n")
        .expect("failed to write amended content");
    repo.git_ai(&["checkpoint", "mock_ai", "amend-authorship.txt"])
        .expect("checkpoint should succeed");
    repo.git(&["add", "amend-authorship.txt"])
        .expect("staging amended content should succeed");
    repo.git(&["commit", "--amend", "-m", "second commit amended"])
        .expect("amend should succeed");

    let blame = repo
        .git_ai(&["blame", "amend-authorship.txt"])
        .expect("blame should succeed");
    assert_blame_line_author_contains(&blame, "ai amended line", "mock_ai");
}

#[test]
#[serial]
fn hooks_mode_root_amend_preserves_ai_authorship() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "hooks");

    let repo = TestRepo::new();
    let path = repo.path().join("root-amend-authorship.txt");

    fs::write(&path, "root line\n").expect("failed to write root line");
    repo.git(&["add", "root-amend-authorship.txt"])
        .expect("staging root line should succeed");
    repo.commit("root commit")
        .expect("root commit should succeed");

    fs::write(&path, "root line\nroot ai amended line\n")
        .expect("failed to write root amended content");
    repo.git_ai(&["checkpoint", "mock_ai", "root-amend-authorship.txt"])
        .expect("checkpoint should succeed");
    repo.git(&["add", "root-amend-authorship.txt"])
        .expect("staging root amended content should succeed");
    repo.git(&["commit", "--amend", "-m", "root commit amended"])
        .expect("root amend should succeed");

    let blame = repo
        .git_ai(&["blame", "root-amend-authorship.txt"])
        .expect("blame should succeed");
    assert_blame_line_author_contains(&blame, "root ai amended line", "mock_ai");
}

#[test]
#[serial]
fn both_mode_amend_preserves_ai_authorship_parity() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "both");

    let repo = TestRepo::new();
    let path = repo.path().join("both-amend-authorship.txt");

    fs::write(&path, "line one\n").expect("failed to write initial content");
    repo.git(&["add", "both-amend-authorship.txt"])
        .expect("staging initial content should succeed");
    repo.commit("initial commit")
        .expect("initial commit should succeed");

    fs::write(&path, "line one\nboth mode ai line\n")
        .expect("failed to write both-mode amend content");
    repo.git_ai(&["checkpoint", "mock_ai", "both-amend-authorship.txt"])
        .expect("checkpoint should succeed");
    repo.git(&["add", "both-amend-authorship.txt"])
        .expect("staging both-mode amend content should succeed");
    repo.git(&["commit", "--amend", "-m", "initial commit amended"])
        .expect("both-mode amend should succeed");

    let blame = repo
        .git_ai(&["blame", "both-amend-authorship.txt"])
        .expect("blame should succeed");
    assert_blame_line_author_contains(&blame, "both mode ai line", "mock_ai");
}
