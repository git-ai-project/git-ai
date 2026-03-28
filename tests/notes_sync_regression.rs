#[macro_use]
#[path = "integration/repos/mod.rs"]
mod repos;

use repos::test_repo::{GitTestMode, real_git_executable};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_temp_path(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}-{}-{nanos}-{seq}", std::process::id()))
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

fn read_note_from_worktree(
    repo_path: &Path,
    commit_sha: &str,
    mode: GitTestMode,
) -> Option<String> {
    repos::test_repo::TestRepo::new_at_path_with_mode(repo_path, mode)
        .read_authorship_note(commit_sha)
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

        let cloned_note = read_note_from_worktree(&clone_dir, &seed_sha, TestRepo::git_mode());
        assert!(
            cloned_note.is_some(),
            "cloned repository should have fetched authorship notes for commit {}",
            seed_sha
        );
    }
}

worktree_test_wrappers! {
    fn notes_sync_clone_relative_target_from_external_cwd_fetches_authorship_notes() {
        // Hooks mode can't intercept clone (no repo exists to have hooks installed)
        if TestRepo::git_mode() == GitTestMode::Hooks {
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

        let cloned_note = read_note_from_worktree(&clone_dir, &seed_sha, TestRepo::git_mode());
        assert!(
            cloned_note.is_some(),
            "cloned repository should have fetched authorship notes for commit {}",
            seed_sha
        );
    }
}

// Regression test: clone from a non-repo directory is the exact scenario that
// triggered the wrapper state timeout bug. The wrapper has worktree=None so no
// wrapper state is sent; the daemon must handle notes sync purely via trace2 events.
worktree_test_wrappers! {
    fn notes_sync_clone_from_non_repo_directory_fetches_authorship_notes() {
        // Hooks mode can't intercept clone (no repo exists to have hooks installed)
        if TestRepo::git_mode() == GitTestMode::Hooks {
            return;
        }

        let (local, upstream) = TestRepo::new_with_remote();

        fs::write(local.path().join("non-repo-clone-seed.txt"), "seed\n")
            .expect("failed to write seed file");
        local
            .git_og(&["add", "non-repo-clone-seed.txt"])
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
                "non-repo-clone-seed-note",
                seed_sha.as_str(),
            ])
            .expect("adding notes should succeed");
        local
            .git_og(&["push", "-u", "origin", "HEAD"])
            .expect("pushing branch should succeed");
        local
            .git_og(&["push", "origin", "refs/notes/ai"])
            .expect("pushing notes should succeed");

        // Clone from a non-repo directory (not inside any git repository).
        // This is the common production scenario and the one that triggers the
        // wrapper state timeout because the wrapper can't determine a worktree.
        let external_cwd = unique_temp_path("notes-sync-clone-non-repo-cwd");
        let _ = fs::remove_dir_all(&external_cwd);
        fs::create_dir_all(&external_cwd).expect("failed to create non-repo cwd");

        let clone_target = "cloned-repo";
        let upstream_str = upstream.path().to_string_lossy().to_string();

        local
            .git_from_working_dir(
                &external_cwd,
                &["clone", upstream_str.as_str(), clone_target],
            )
            .expect("clone from non-repo cwd should succeed");

        let clone_dir = external_cwd.join(clone_target);
        assert!(
            clone_dir.exists(),
            "clone target should exist at {}",
            clone_dir.display()
        );

        let cloned_note = read_note_from_worktree(&clone_dir, &seed_sha, TestRepo::git_mode());
        assert!(
            cloned_note.is_some(),
            "cloned repository should have fetched authorship notes for commit {} (clone from non-repo directory)",
            seed_sha
        );
    }
}

// Regression test: clone with an absolute target path from a non-repo CWD.
// Exercises the side-effect target resolution path where the clone target is
// specified as an absolute path (common in scripted / CI workflows and when
// the user types `git clone URL /some/absolute/path`).
worktree_test_wrappers! {
    fn notes_sync_clone_absolute_target_from_non_repo_cwd_fetches_authorship_notes() {
        if TestRepo::git_mode() == GitTestMode::Hooks {
            return;
        }

        let (local, upstream) = TestRepo::new_with_remote();

        fs::write(local.path().join("abs-clone-seed.txt"), "seed\n")
            .expect("failed to write seed file");
        local
            .git_og(&["add", "abs-clone-seed.txt"])
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
                "abs-clone-note",
                seed_sha.as_str(),
            ])
            .expect("adding notes should succeed");
        local
            .git_og(&["push", "-u", "origin", "HEAD"])
            .expect("pushing branch should succeed");
        local
            .git_og(&["push", "origin", "refs/notes/ai"])
            .expect("pushing notes should succeed");

        // Clone from a non-repo directory using an absolute target path.
        let external_cwd = unique_temp_path("notes-sync-abs-target-cwd");
        let _ = fs::remove_dir_all(&external_cwd);
        fs::create_dir_all(&external_cwd).expect("failed to create external cwd");

        let clone_dir = unique_temp_path("notes-sync-abs-target-clone");
        let _ = fs::remove_dir_all(&clone_dir);
        let clone_dir_str = clone_dir.to_string_lossy().to_string();
        let upstream_str = upstream.path().to_string_lossy().to_string();

        local
            .git_from_working_dir(
                &external_cwd,
                &["clone", upstream_str.as_str(), clone_dir_str.as_str()],
            )
            .expect("clone with absolute target should succeed");

        assert!(
            clone_dir.exists(),
            "clone target should exist at {}",
            clone_dir.display()
        );

        let cloned_note = read_note_from_worktree(&clone_dir, &seed_sha, TestRepo::git_mode());
        assert!(
            cloned_note.is_some(),
            "cloned repository should have fetched authorship notes for commit {} (absolute target from non-repo CWD)",
            seed_sha
        );
    }
}

// Regression test: clone with NO explicit target directory (implicit target
// derived from the source URL/path).  This is the common user scenario:
//   cd ~/projects && git clone https://github.com/user/repo
// In trace2, the root process emits def_repo with the correct clone destination,
// but child processes (remote-https, index-pack) emit def_repo with the CWD as
// worktree.  The normalizer must prefer the root def_repo and ignore children.
worktree_test_wrappers! {
    fn notes_sync_clone_implicit_target_from_non_repo_cwd_fetches_authorship_notes() {
        if TestRepo::git_mode() == GitTestMode::Hooks {
            return;
        }

        let (local, upstream) = TestRepo::new_with_remote();

        fs::write(local.path().join("implicit-seed.txt"), "seed\n")
            .expect("failed to write seed file");
        local
            .git_og(&["add", "implicit-seed.txt"])
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
                "implicit-clone-note",
                seed_sha.as_str(),
            ])
            .expect("adding notes should succeed");
        local
            .git_og(&["push", "-u", "origin", "HEAD"])
            .expect("pushing branch should succeed");
        local
            .git_og(&["push", "origin", "refs/notes/ai"])
            .expect("pushing notes should succeed");

        // Clone from a non-repo CWD with NO explicit target — git derives the
        // directory name from the source path (the upstream bare repo's basename).
        let external_cwd = unique_temp_path("notes-sync-clone-implicit-cwd");
        let _ = fs::remove_dir_all(&external_cwd);
        fs::create_dir_all(&external_cwd).expect("failed to create external cwd");

        let upstream_str = upstream.path().to_string_lossy().to_string();
        // Derive the expected directory name the same way git does: basename of the source.
        let expected_dir_name = Path::new(&upstream_str)
            .file_name()
            .expect("upstream path should have a filename")
            .to_string_lossy()
            .to_string();
        // Strip .git suffix if present (matches git's behavior)
        let expected_dir_name = expected_dir_name
            .strip_suffix(".git")
            .unwrap_or(&expected_dir_name);

        local
            .git_from_working_dir(&external_cwd, &["clone", upstream_str.as_str()])
            .expect("clone with implicit target should succeed");

        let clone_dir = external_cwd.join(expected_dir_name);
        assert!(
            clone_dir.exists(),
            "implicit clone target should exist at {}",
            clone_dir.display()
        );

        let cloned_note = read_note_from_worktree(&clone_dir, &seed_sha, TestRepo::git_mode());
        assert!(
            cloned_note.is_some(),
            "cloned repository should have fetched authorship notes for commit {} (implicit target from non-repo CWD)",
            seed_sha
        );
    }
}

worktree_test_wrappers! {
    fn notes_sync_fetch_does_not_import_authorship_notes() {
        let mode = TestRepo::git_mode();
        if mode == GitTestMode::Hooks {
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
            local.read_authorship_note(&seed_sha).is_none(),
            "local note should be absent before fetch"
        );

        local
            .git(&["fetch", "origin"])
            .expect("fetch should succeed");

        let fetched_note = local.read_authorship_note(&seed_sha);
        match mode {
            GitTestMode::Daemon | GitTestMode::WrapperDaemon | GitTestMode::Wrapper | GitTestMode::Both => assert!(
                fetched_note.is_none(),
                "plain git fetch should not import authorship note for commit {} in {:?} mode",
                seed_sha,
                mode
            ),
            GitTestMode::Hooks => unreachable!("hooks mode returned above"),
        }
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
            local.read_authorship_note(&remote_sha).is_none(),
            "local note should be absent before pull"
        );

        local
            .git(&["pull", "--ff-only", "origin", default_branch.as_str()])
            .expect("pull --ff-only should succeed");

        let pulled_note = local.read_authorship_note(&remote_sha);
        assert!(
            pulled_note.is_some(),
            "pull should import authorship note for remote commit {}",
            remote_sha
        );
    }
}

worktree_test_wrappers! {
    fn notes_sync_pull_fast_forward_syncs_only_selected_remote() {
        let (local, upstream) = TestRepo::new_with_remote();
        let backup = repos::test_repo::TestRepo::new_bare_with_mode(TestRepo::git_mode());
        let default_branch = local.current_branch();

        fs::write(local.path().join("pull-base.txt"), "base\n")
            .expect("failed to write pull base file");
        local
            .git_og(&["add", "pull-base.txt"])
            .expect("add should succeed");
        local
            .git_og(&["commit", "-m", "base commit"])
            .expect("base commit should succeed");

        let base_sha = local
            .git_og(&["rev-parse", "HEAD"])
            .expect("rev-parse should succeed")
            .trim()
            .to_string();

        local
            .git_og(&["push", "-u", "origin", "HEAD"])
            .expect("initial push to origin should succeed");

        let backup_path = backup.path().to_string_lossy().to_string();
        local
            .git_og(&["remote", "add", "backup", backup_path.as_str()])
            .expect("adding backup remote should succeed");
        local
            .git_og(&["push", "backup", "HEAD"])
            .expect("initial push to backup should succeed");

        let backup_clone = unique_temp_path("notes-sync-pull-backup-remote");
        let backup_clone_str = backup_clone.to_string_lossy().to_string();
        let _ = fs::remove_dir_all(&backup_clone);

        run_git(&["clone", backup_path.as_str(), backup_clone_str.as_str()]);
        run_git(&[
            "-C",
            backup_clone_str.as_str(),
            "config",
            "user.name",
            "Test User",
        ]);
        run_git(&[
            "-C",
            backup_clone_str.as_str(),
            "config",
            "user.email",
            "test@example.com",
        ]);
        run_git(&[
            "-C",
            backup_clone_str.as_str(),
            "notes",
            "--ref=ai",
            "add",
            "-m",
            "backup-remote-note",
            base_sha.as_str(),
        ]);
        run_git(&[
            "-C",
            backup_clone_str.as_str(),
            "push",
            "origin",
            "refs/notes/ai",
        ]);

        let origin_clone = unique_temp_path("notes-sync-pull-origin-remote");
        let origin_clone_str = origin_clone.to_string_lossy().to_string();
        let upstream_str = upstream.path().to_string_lossy().to_string();
        let _ = fs::remove_dir_all(&origin_clone);

        run_git(&["clone", upstream_str.as_str(), origin_clone_str.as_str()]);
        run_git(&[
            "-C",
            origin_clone_str.as_str(),
            "config",
            "user.name",
            "Test User",
        ]);
        run_git(&[
            "-C",
            origin_clone_str.as_str(),
            "config",
            "user.email",
            "test@example.com",
        ]);

        fs::write(origin_clone.join("pull-selected-remote.txt"), "remote\n")
            .expect("failed to write selected remote file");
        run_git(&["-C", origin_clone_str.as_str(), "add", "pull-selected-remote.txt"]);
        run_git(&[
            "-C",
            origin_clone_str.as_str(),
            "commit",
            "-m",
            "remote pull commit",
        ]);

        let remote_sha = run_git(&["-C", origin_clone_str.as_str(), "rev-parse", "HEAD"]);

        run_git(&[
            "-C",
            origin_clone_str.as_str(),
            "notes",
            "--ref=ai",
            "add",
            "-m",
            "origin-remote-note",
            remote_sha.as_str(),
        ]);
        run_git(&[
            "-C",
            origin_clone_str.as_str(),
            "push",
            "origin",
            default_branch.as_str(),
        ]);
        run_git(&[
            "-C",
            origin_clone_str.as_str(),
            "push",
            "origin",
            "refs/notes/ai",
        ]);

        assert!(
            local.read_authorship_note(&base_sha).is_none(),
            "backup remote note should be absent before pull"
        );
        assert!(
            local.read_authorship_note(&remote_sha).is_none(),
            "origin remote note should be absent before pull"
        );

        local
            .git(&["pull", "--ff-only", "origin", default_branch.as_str()])
            .expect("pull --ff-only should succeed");

        let pulled_origin_note = local.read_authorship_note(&remote_sha);
        assert!(
            pulled_origin_note.is_some(),
            "pull should import authorship note for selected remote commit {}",
            remote_sha
        );

        let leaked_backup_note = local.read_authorship_note(&base_sha);
        assert!(
            leaked_backup_note.is_none(),
            "pull from origin should not import backup remote note for commit {}",
            base_sha
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
            local.read_authorship_note(&remote_sha).is_none(),
            "local note should be absent before pull --rebase"
        );

        local
            .git(&["pull", "--rebase", "origin", default_branch.as_str()])
            .expect("pull --rebase should succeed");

        let pulled_note = local.read_authorship_note(&remote_sha);
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

        let remote_note = local.read_authorship_note_in_git_dir(upstream.path(), &seed_sha);
        assert!(
            remote_note.is_some(),
            "push should propagate authorship note for commit {} to upstream",
            seed_sha
        );
    }
}
