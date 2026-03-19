#[macro_use]
mod repos;

use git_ai::daemon::{ControlRequest, send_control_request};
use repos::test_repo::{GitTestMode, TestRepo, real_git_executable};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn unique_temp_path(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
}

fn run_git(args: &[&str]) -> String {
    let output = Command::new(real_git_executable())
        .args(args)
        .output()
        .expect("git command should execute");

    assert!(
        output.status.success(),
        "git {} failed:\nstdout: {}\nstderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn daemon_control_socket_path(repo: &TestRepo) -> PathBuf {
    repo.daemon_control_socket_path()
}

fn sync_daemon_repo_if_needed(mode: GitTestMode, repo: &TestRepo, repo_working_dir: &Path) {
    if !mode.uses_daemon() {
        return;
    }

    let socket_path = daemon_control_socket_path(repo);
    let repo_working_dir = repo_working_dir.to_string_lossy().to_string();
    let mut last_latest_seq = 0_u64;
    let mut stable_idle_polls = 0_u8;
    let mut saw_activity = false;

    for _ in 0..800 {
        let status = send_control_request(
            &socket_path,
            &ControlRequest::StatusFamily {
                repo_working_dir: repo_working_dir.clone(),
            },
        )
        .unwrap_or_else(|err| {
            panic!(
                "daemon status request failed for {} via {}: {}",
                repo_working_dir,
                socket_path.display(),
                err
            )
        });
        assert!(
            status.ok,
            "daemon status failed for {}: {}",
            repo_working_dir,
            status
                .error
                .clone()
                .unwrap_or_else(|| "unknown daemon status error".to_string())
        );

        let latest_seq = status
            .data
            .as_ref()
            .and_then(|v| v.get("latest_seq"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if latest_seq > 0 {
            saw_activity = true;
        }
        let backlog = status
            .data
            .as_ref()
            .and_then(|v| v.get("backlog"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let pending_roots = status
            .data
            .as_ref()
            .and_then(|v| v.get("pending_roots"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);

        if latest_seq > 0 {
            let barrier = send_control_request(
                &socket_path,
                &ControlRequest::BarrierAppliedThroughSeq {
                    repo_working_dir: repo_working_dir.clone(),
                    seq: latest_seq,
                },
            )
            .unwrap_or_else(|err| {
                panic!(
                    "daemon barrier request failed for {} via {}: {}",
                    repo_working_dir,
                    socket_path.display(),
                    err
                )
            });
            assert!(
                barrier.ok,
                "daemon barrier failed for {}: {}",
                repo_working_dir,
                barrier
                    .error
                    .clone()
                    .unwrap_or_else(|| "unknown daemon barrier error".to_string())
            );

            let applied_seq = barrier.applied_seq.unwrap_or(0);
            if applied_seq < latest_seq {
                stable_idle_polls = 0;
                thread::sleep(Duration::from_millis(10));
                continue;
            }
        }

        if saw_activity
            && backlog == 0
            && pending_roots == 0
            && latest_seq == last_latest_seq
        {
            stable_idle_polls = stable_idle_polls.saturating_add(1);
            if stable_idle_polls >= 2 {
                return;
            }
        } else {
            stable_idle_polls = 0;
        }

        last_latest_seq = latest_seq;
        thread::sleep(Duration::from_millis(10));
    }

    panic!(
        "daemon did not settle for repo {} via {} (saw_activity={})",
        repo_working_dir,
        socket_path.display(),
        saw_activity
    );
}

fn read_note_from_worktree(repo_path: &Path, commit_sha: &str) -> Option<String> {
    let repo_path_str = repo_path.to_string_lossy().to_string();
    let output = Command::new(real_git_executable())
        .args([
            "-C",
            repo_path_str.as_str(),
            "notes",
            "--ref=ai",
            "show",
            commit_sha,
        ])
        .output()
        .expect("git notes show should execute");

    if !output.status.success() {
        return None;
    }

    let note = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if note.is_empty() { None } else { Some(note) }
}

fn read_note_from_bare_repo(git_dir: &Path, commit_sha: &str) -> Option<String> {
    let git_dir_str = git_dir.to_string_lossy().to_string();
    let output = Command::new(real_git_executable())
        .args([
            "--git-dir",
            git_dir_str.as_str(),
            "notes",
            "--ref=ai",
            "show",
            commit_sha,
        ])
        .output()
        .expect("git notes show on bare repo should execute");

    if !output.status.success() {
        return None;
    }

    let note = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if note.is_empty() { None } else { Some(note) }
}

worktree_test_wrappers! {
    fn notes_sync_clone_fetches_authorship_notes_from_origin() {
        if TestRepo::git_mode() == GitTestMode::Hooks {
            return;
        }

        let (local, upstream) = TestRepo::new_with_remote();

        fs::write(local.path().join("clone-seed.txt"), "seed\n")
            .expect("failed to write clone seed file");
        local
            .git_og(&["add", "clone-seed.txt"])
            .expect("add should succeed");
        local
            .git_og(&["commit", "-m", "seed commit"])
            .expect("seed commit should succeed");

        let seed_sha = local
            .git_og(&["rev-parse", "HEAD"])
            .expect("rev-parse should succeed")
            .trim()
            .to_string();

        local
            .git_og(&[
                "notes",
                "--ref=ai",
                "add",
                "-m",
                "clone-seed-note",
                seed_sha.as_str(),
            ])
            .expect("adding notes should succeed");
        local
            .git_og(&["push", "-u", "origin", "HEAD"])
            .expect("pushing branch should succeed");
        local
            .git_og(&["push", "origin", "refs/notes/ai"])
            .expect("pushing notes should succeed");

        let clone_dir = unique_temp_path("notes-sync-clone");
        let clone_dir_str = clone_dir.to_string_lossy().to_string();
        let upstream_str = upstream.path().to_string_lossy().to_string();
        let _ = fs::remove_dir_all(&clone_dir);

        local
            .git(&["clone", upstream_str.as_str(), clone_dir_str.as_str()])
            .expect("clone should succeed");

        sync_daemon_repo_if_needed(TestRepo::git_mode(), &local, &clone_dir);

        let cloned_note = read_note_from_worktree(&clone_dir, &seed_sha);
        assert!(
            cloned_note.is_some(),
            "cloned repository should have fetched authorship notes for commit {}",
            seed_sha
        );
    }
}

worktree_test_wrappers! {
    fn notes_sync_clone_relative_target_from_external_cwd_fetches_authorship_notes() {
        if TestRepo::git_mode() != GitTestMode::Daemon {
            return;
        }

        let (local, upstream) = TestRepo::new_with_remote();

        fs::write(local.path().join("clone-relative-seed.txt"), "seed\n")
            .expect("failed to write clone-relative seed file");
        local
            .git_og(&["add", "clone-relative-seed.txt"])
            .expect("add should succeed");
        local
            .git_og(&["commit", "-m", "seed commit"])
            .expect("seed commit should succeed");

        let seed_sha = local
            .git_og(&["rev-parse", "HEAD"])
            .expect("rev-parse should succeed")
            .trim()
            .to_string();

        local
            .git_og(&[
                "notes",
                "--ref=ai",
                "add",
                "-m",
                "clone-relative-seed-note",
                seed_sha.as_str(),
            ])
            .expect("adding notes should succeed");
        local
            .git_og(&["push", "-u", "origin", "HEAD"])
            .expect("pushing branch should succeed");
        local
            .git_og(&["push", "origin", "refs/notes/ai"])
            .expect("pushing notes should succeed");

        let external_cwd = unique_temp_path("notes-sync-clone-relative-cwd");
        let _ = fs::remove_dir_all(&external_cwd);
        fs::create_dir_all(&external_cwd).expect("failed to create external cwd");

        let relative_target = "nested/relative-clone";
        let upstream_str = upstream.path().to_string_lossy().to_string();

        local
            .git_from_working_dir(&external_cwd, &["clone", upstream_str.as_str(), relative_target])
            .expect("clone from external cwd should succeed");

        let clone_dir = external_cwd.join(relative_target);
        assert!(
            clone_dir.exists(),
            "relative clone target should exist at {}",
            clone_dir.display()
        );

        sync_daemon_repo_if_needed(TestRepo::git_mode(), &local, &clone_dir);

        let cloned_note = read_note_from_worktree(&clone_dir, &seed_sha);
        assert!(
            cloned_note.is_some(),
            "cloned repository should have fetched authorship notes for commit {}",
            seed_sha
        );
    }
}

worktree_test_wrappers! {
    fn notes_sync_fetch_imports_authorship_notes_from_remote() {
        if TestRepo::git_mode() == GitTestMode::Hooks {
            return;
        }

        let (local, _upstream) = TestRepo::new_with_remote();

        fs::write(local.path().join("fetch-seed.txt"), "seed\n")
            .expect("failed to write fetch seed file");
        local
            .git_og(&["add", "fetch-seed.txt"])
            .expect("add should succeed");
        local
            .git_og(&["commit", "-m", "seed commit"])
            .expect("seed commit should succeed");

        let seed_sha = local
            .git_og(&["rev-parse", "HEAD"])
            .expect("rev-parse should succeed")
            .trim()
            .to_string();

        local
            .git_og(&[
                "notes",
                "--ref=ai",
                "add",
                "-m",
                "fetch-seed-note",
                seed_sha.as_str(),
            ])
            .expect("adding notes should succeed");
        local
            .git_og(&["push", "-u", "origin", "HEAD"])
            .expect("pushing branch should succeed");
        local
            .git_og(&["push", "origin", "refs/notes/ai"])
            .expect("pushing notes should succeed");

        let _ = local.git_og(&["update-ref", "-d", "refs/notes/ai"]);
        assert!(
            read_note_from_worktree(local.path(), &seed_sha).is_none(),
            "local note should be absent before fetch"
        );

        local
            .git(&["fetch", "origin"])
            .expect("fetch should succeed");

        sync_daemon_repo_if_needed(TestRepo::git_mode(), &local, local.path());

        let fetched_note = read_note_from_worktree(local.path(), &seed_sha);
        assert!(
            fetched_note.is_some(),
            "fetch should import authorship note for commit {}",
            seed_sha
        );
    }
}

worktree_test_wrappers! {
    fn notes_sync_pull_fast_forward_imports_authorship_notes() {
        let (local, upstream) = TestRepo::new_with_remote();
        let default_branch = local.current_branch();

        fs::write(local.path().join("pull-base.txt"), "base\n")
            .expect("failed to write pull base file");
        local
            .git_og(&["add", "pull-base.txt"])
            .expect("add should succeed");
        local
            .git_og(&["commit", "-m", "base commit"])
            .expect("base commit should succeed");
        local
            .git_og(&["push", "-u", "origin", "HEAD"])
            .expect("initial push should succeed");

        let remote_clone = unique_temp_path("notes-sync-pull-remote");
        let remote_clone_str = remote_clone.to_string_lossy().to_string();
        let upstream_str = upstream.path().to_string_lossy().to_string();
        let _ = fs::remove_dir_all(&remote_clone);

        run_git(&["clone", upstream_str.as_str(), remote_clone_str.as_str()]);
        run_git(&[
            "-C",
            remote_clone_str.as_str(),
            "config",
            "user.name",
            "Test User",
        ]);
        run_git(&[
            "-C",
            remote_clone_str.as_str(),
            "config",
            "user.email",
            "test@example.com",
        ]);

        fs::write(remote_clone.join("pull-remote.txt"), "remote\n")
            .expect("failed to write remote pull file");
        run_git(&["-C", remote_clone_str.as_str(), "add", "pull-remote.txt"]);
        run_git(&[
            "-C",
            remote_clone_str.as_str(),
            "commit",
            "-m",
            "remote pull commit",
        ]);

        let remote_sha = run_git(&["-C", remote_clone_str.as_str(), "rev-parse", "HEAD"]);

        run_git(&[
            "-C",
            remote_clone_str.as_str(),
            "notes",
            "--ref=ai",
            "add",
            "-m",
            "pull-remote-note",
            remote_sha.as_str(),
        ]);
        run_git(&[
            "-C",
            remote_clone_str.as_str(),
            "push",
            "origin",
            default_branch.as_str(),
        ]);
        run_git(&[
            "-C",
            remote_clone_str.as_str(),
            "push",
            "origin",
            "refs/notes/ai",
        ]);

        assert!(
            read_note_from_worktree(local.path(), &remote_sha).is_none(),
            "local note should be absent before pull"
        );

        local
            .git(&["pull", "--ff-only", "origin", default_branch.as_str()])
            .expect("pull --ff-only should succeed");

        sync_daemon_repo_if_needed(TestRepo::git_mode(), &local, local.path());

        let pulled_note = read_note_from_worktree(local.path(), &remote_sha);
        assert!(
            pulled_note.is_some(),
            "pull should import authorship note for remote commit {}",
            remote_sha
        );
    }
}

worktree_test_wrappers! {
    fn notes_sync_pull_rebase_imports_authorship_notes() {
        let (local, upstream) = TestRepo::new_with_remote();
        let default_branch = local.current_branch();

        fs::write(local.path().join("rebase-base.txt"), "base\n")
            .expect("failed to write rebase base file");
        local
            .git_og(&["add", "rebase-base.txt"])
            .expect("add should succeed");
        local
            .git_og(&["commit", "-m", "base commit"])
            .expect("base commit should succeed");
        local
            .git_og(&["push", "-u", "origin", "HEAD"])
            .expect("initial push should succeed");

        fs::write(local.path().join("local-only.txt"), "local\n")
            .expect("failed to write local-only file");
        local
            .git_og(&["add", "local-only.txt"])
            .expect("add local-only should succeed");
        local
            .git_og(&["commit", "-m", "local commit"])
            .expect("local commit should succeed");

        let remote_clone = unique_temp_path("notes-sync-rebase-remote");
        let remote_clone_str = remote_clone.to_string_lossy().to_string();
        let upstream_str = upstream.path().to_string_lossy().to_string();
        let _ = fs::remove_dir_all(&remote_clone);

        run_git(&["clone", upstream_str.as_str(), remote_clone_str.as_str()]);
        run_git(&[
            "-C",
            remote_clone_str.as_str(),
            "config",
            "user.name",
            "Test User",
        ]);
        run_git(&[
            "-C",
            remote_clone_str.as_str(),
            "config",
            "user.email",
            "test@example.com",
        ]);

        fs::write(remote_clone.join("remote-only.txt"), "remote\n")
            .expect("failed to write remote-only file");
        run_git(&["-C", remote_clone_str.as_str(), "add", "remote-only.txt"]);
        run_git(&[
            "-C",
            remote_clone_str.as_str(),
            "commit",
            "-m",
            "remote commit",
        ]);

        let remote_sha = run_git(&["-C", remote_clone_str.as_str(), "rev-parse", "HEAD"]);

        run_git(&[
            "-C",
            remote_clone_str.as_str(),
            "notes",
            "--ref=ai",
            "add",
            "-m",
            "pull-rebase-remote-note",
            remote_sha.as_str(),
        ]);
        run_git(&[
            "-C",
            remote_clone_str.as_str(),
            "push",
            "origin",
            default_branch.as_str(),
        ]);
        run_git(&[
            "-C",
            remote_clone_str.as_str(),
            "push",
            "origin",
            "refs/notes/ai",
        ]);

        assert!(
            read_note_from_worktree(local.path(), &remote_sha).is_none(),
            "local note should be absent before pull --rebase"
        );

        local
            .git(&["pull", "--rebase", "origin", default_branch.as_str()])
            .expect("pull --rebase should succeed");

        sync_daemon_repo_if_needed(TestRepo::git_mode(), &local, local.path());

        let pulled_note = read_note_from_worktree(local.path(), &remote_sha);
        assert!(
            pulled_note.is_some(),
            "pull --rebase should import authorship note for remote commit {}",
            remote_sha
        );
    }
}

worktree_test_wrappers! {
    fn notes_sync_push_propagates_authorship_notes_to_remote() {
        let (local, upstream) = TestRepo::new_with_remote();

        fs::write(local.path().join("push-seed.txt"), "seed\n")
            .expect("failed to write push seed file");
        local
            .git_og(&["add", "push-seed.txt"])
            .expect("add should succeed");
        local
            .git_og(&["commit", "-m", "seed commit"])
            .expect("seed commit should succeed");

        let seed_sha = local
            .git_og(&["rev-parse", "HEAD"])
            .expect("rev-parse should succeed")
            .trim()
            .to_string();

        local
            .git_og(&[
                "notes",
                "--ref=ai",
                "add",
                "-m",
                "push-seed-note",
                seed_sha.as_str(),
            ])
            .expect("adding notes should succeed");

        local
            .git(&["push", "-u", "origin", "HEAD"])
            .expect("push should succeed");

        sync_daemon_repo_if_needed(TestRepo::git_mode(), &local, local.path());

        let remote_note = read_note_from_bare_repo(upstream.path(), &seed_sha);
        assert!(
            remote_note.is_some(),
            "push should propagate authorship note for commit {} to upstream",
            seed_sha
        );
    }
}
