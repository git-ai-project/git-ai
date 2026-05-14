#![allow(dead_code)]

use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::core::working_log::{Checkpoint, CheckpointKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use super::test_file::TestFile;

// ---------------------------------------------------------------------------
// ConfigPatch — serialized to JSON and passed via GIT_AI_TEST_CONFIG_PATCH
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ConfigPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_prompts_in_repositories: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_storage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature_flags: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_attributes: Option<HashMap<String, String>>,
}

// ---------------------------------------------------------------------------
// WorkingLogs — handle for reading working log data from .git/ai/working_logs/
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WorkingLogs {
    pub dir: PathBuf,
}

impl WorkingLogs {
    /// Read all checkpoints from the checkpoints.jsonl file in this working log dir.
    pub fn read_all_checkpoints(&self) -> Result<Vec<Checkpoint>, String> {
        let checkpoints_file = self.dir.join("checkpoints.jsonl");
        if !checkpoints_file.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&checkpoints_file)
            .map_err(|e| format!("Failed to read checkpoints.jsonl: {}", e))?;
        let mut checkpoints = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Checkpoint>(line) {
                Ok(cp) => checkpoints.push(cp),
                Err(_) => continue,
            }
        }
        Ok(checkpoints)
    }

    /// Return all file paths that have been touched by an AI checkpoint.
    pub fn all_ai_touched_files(&self) -> Result<Vec<String>, String> {
        let checkpoints = self.read_all_checkpoints()?;
        let mut files: Vec<String> = Vec::new();
        for cp in &checkpoints {
            if cp.kind == CheckpointKind::AiAgent {
                for entry in &cp.entries {
                    if !files.contains(&entry.file) {
                        files.push(entry.file.clone());
                    }
                }
            }
        }
        Ok(files)
    }
}

// ---------------------------------------------------------------------------
// Binary compilation (cached across all tests in the process)
// ---------------------------------------------------------------------------

static COMPILED_BINARY: OnceLock<PathBuf> = OnceLock::new();

fn compile_binary() -> PathBuf {
    if let Ok(override_path) = std::env::var("GIT_AI_TEST_BINARY_PATH") {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            return path;
        }
        panic!(
            "GIT_AI_TEST_BINARY_PATH does not point to a file: {}",
            path.display()
        );
    }

    println!("Compiling git-ai binary for tests...");

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("cargo")
        .args(["build", "--bin", "git-ai", "--features", "test-support"])
        .current_dir(manifest_dir)
        .output()
        .expect("Failed to compile git-ai binary");

    if !output.status.success() {
        panic!(
            "Failed to compile git-ai:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let target_dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| {
        PathBuf::from(manifest_dir)
            .join("target")
            .to_string_lossy()
            .into_owned()
    });
    #[cfg(windows)]
    let binary_path = PathBuf::from(&target_dir).join("debug/git-ai.exe");
    #[cfg(not(windows))]
    let binary_path = PathBuf::from(&target_dir).join("debug/git-ai");

    binary_path
}

pub fn get_binary_path() -> &'static PathBuf {
    COMPILED_BINARY.get_or_init(compile_binary)
}

/// Returns the default branch name used in test repos.
pub fn default_branchname() -> &'static str {
    "main"
}

// ---------------------------------------------------------------------------
// Real git executable discovery
// ---------------------------------------------------------------------------

fn find_real_git() -> &'static str {
    static REAL_GIT: OnceLock<String> = OnceLock::new();
    REAL_GIT.get_or_init(|| {
        let candidates: &[&str] = &[
            "/usr/bin/git",
            "/usr/local/bin/git",
            "/opt/homebrew/bin/git",
            "/bin/git",
        ];
        for c in candidates {
            if Path::new(c).is_file() {
                return c.to_string();
            }
        }
        "git".to_string()
    })
}

pub fn real_git_executable() -> &'static str {
    find_real_git()
}

// ---------------------------------------------------------------------------
// with_worktree_mode (no-op stub for reuse_tests_in_worktree macro)
// ---------------------------------------------------------------------------

pub fn with_worktree_mode<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    f()
}

// ---------------------------------------------------------------------------
// NewCommit
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct NewCommit {
    pub authorship_log: AuthorshipLog,
    pub stdout: String,
    pub commit_sha: String,
}

impl NewCommit {
    pub fn print_authorship(&self) {
        println!("{}", self.authorship_log.serialize_to_string());
    }
}

// ---------------------------------------------------------------------------
// TestRepo
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct TestRepo {
    path: PathBuf,
    _tempdir: tempfile::TempDir,
    /// Additional env vars passed to all git-ai subprocess invocations.
    daemon_env: Vec<(String, String)>,
    /// Config patch applied via GIT_AI_TEST_CONFIG_PATCH env var.
    config_patch: Option<ConfigPatch>,
}

impl TestRepo {
    pub fn new() -> Self {
        let tempdir = tempfile::tempdir().expect("failed to create temp dir");
        let path = tempdir.path().to_path_buf();

        let git = real_git_executable();
        let p = path.to_str().unwrap();

        let output = Command::new(git)
            .args(["init", p])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .expect("failed to init test repo");
        assert!(output.status.success(), "git init failed");

        for args in [
            vec!["-C", p, "config", "user.name", "Test User"],
            vec!["-C", p, "config", "user.email", "test@example.com"],
            vec!["-C", p, "symbolic-ref", "HEAD", "refs/heads/main"],
        ] {
            let output = Command::new(git)
                .args(&args)
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .expect("failed to configure test repo");
            assert!(output.status.success(), "git config failed: {:?}", args);
        }

        Self {
            path,
            _tempdir: tempdir,
            daemon_env: Vec::new(),
            config_patch: None,
        }
    }

    /// Create a TestRepo with additional env vars passed to all git-ai subprocess invocations.
    pub fn new_with_daemon_env(env_vars: &[(&str, &str)]) -> Self {
        let mut repo = Self::new();
        repo.daemon_env = env_vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        repo
    }

    /// Apply config patches. Modifies the GIT_AI_TEST_CONFIG_PATCH env var for subsequent calls.
    pub fn patch_git_ai_config(&mut self, f: impl FnOnce(&mut ConfigPatch)) {
        let mut patch = self.config_patch.clone().unwrap_or_default();
        f(&mut patch);
        self.config_patch = Some(patch);
    }

    /// Return the serialized config patch JSON, if any.
    pub fn config_patch_json(&self) -> Option<String> {
        self.config_patch
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap())
    }

    /// Return a WorkingLogs handle for the current HEAD's working log directory.
    pub fn current_working_logs(&self) -> WorkingLogs {
        let git = real_git_executable();
        let head_sha = Command::new(git)
            .current_dir(&self.path)
            .args(["rev-parse", "HEAD"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "initial".to_string());

        let dir = self.path.join(".git").join("ai").join("working_logs").join(&head_sha);
        WorkingLogs { dir }
    }

    /// Return the test HOME directory path.
    pub fn test_home_path(&self) -> PathBuf {
        self._tempdir.path().to_path_buf()
    }

    /// Return the test DB path (sibling to the repo directory).
    pub fn test_db_path(&self) -> PathBuf {
        self._tempdir.path().join("test.db")
    }

    /// Read an authorship note from a specific git dir (e.g., a bare remote repo).
    pub fn read_authorship_note_in_git_dir(&self, git_dir: &Path, commit_sha: &str) -> Option<String> {
        let git = real_git_executable();
        let output = Command::new(git)
            .args(["--git-dir", git_dir.to_str().unwrap(), "notes", "--ref=ai", "show", commit_sha])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .ok()?;
        if output.status.success() {
            let content = String::from_utf8_lossy(&output.stdout).to_string();
            if content.trim().is_empty() {
                None
            } else {
                Some(content)
            }
        } else {
            None
        }
    }

    /// Synchronize/flush the daemon. In v2 test mode, checkpoints are processed
    /// synchronously, so this is a no-op.
    pub fn sync_daemon(&self) {
        // In v2, checkpoints are processed synchronously but post-commit
        // needs to be triggered explicitly when tests use repo.git(&["commit", ...])
        // followed by sync_daemon() (the pattern used by copilot/agent tests).
        //
        // Only run post-commit if:
        // 1. HEAD has no authorship note yet, AND
        // 2. There's a working log for the parent commit (i.e., checkpoints exist to consume)
        let git = real_git_executable();
        let has_note = Command::new(git)
            .current_dir(&self.path)
            .args(["notes", "--ref=ai", "show", "HEAD"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .ok()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if has_note {
            return;
        }

        // Check if there's a working log for the parent of HEAD
        let parent_sha = Command::new(git)
            .current_dir(&self.path)
            .args(["rev-parse", "HEAD~1"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

        let git_dir = self.path.join(".git");
        let parent_key = parent_sha.as_deref().unwrap_or("initial");
        let working_log_dir = git_dir.join("ai").join("working_logs").join(parent_key);
        if working_log_dir.exists() {
            let _ = self.git_ai(&["post-commit"]);
        }
    }

    /// Force-synchronize the daemon. Also calls post-commit in v2 test mode.
    pub fn sync_daemon_force(&self) {
        let _ = self.git_ai(&["post-commit"]);
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Run a real git command directly (no proxy, no wrapper).
    /// If the command is a rebase, automatically handles post-rewrite note copying.
    /// If the command is `commit --amend`, automatically runs `git-ai post-commit` after.
    /// If the command is `pull` or `fetch`, automatically fetches notes from the remote.
    /// If the command is `pull --rebase`, also handles post-rewrite note copying.
    pub fn git(&self, args: &[&str]) -> Result<String, String> {
        let git = real_git_executable();

        // Detect pull/fetch commands
        let is_pull = args.first().map(|a| *a == "pull").unwrap_or(false);
        let is_fetch = args.first().map(|a| *a == "fetch").unwrap_or(false);
        let is_pull_rebase_flag = is_pull
            && args.iter().any(|a| *a == "--rebase" || a.starts_with("--rebase="));
        // Also detect pull.rebase=true from git config (implicit rebase)
        let is_pull_rebase = if is_pull && !is_pull_rebase_flag {
            is_pull_rebase_flag || self.has_pull_rebase_config()
        } else {
            is_pull_rebase_flag
        };

        // Determine the remote name from args (for pull/fetch), defaulting to "origin"
        let pull_fetch_remote = if is_pull || is_fetch {
            self.detect_remote_from_args(args).unwrap_or_else(|| "origin".to_string())
        } else {
            String::new()
        };

        // Capture pre-pull HEAD for working log migration (FF pull case)
        let pre_pull_head = if is_pull {
            Command::new(git)
                .current_dir(&self.path)
                .args(["rev-parse", "HEAD"])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            None
        };

        // Detect if autostash will be used (explicit flag or config)
        let is_autostash = is_pull
            && (args.iter().any(|a| *a == "--autostash")
                || (is_pull_rebase && self.has_rebase_autostash_config()));
        // Check if there are dirty changes that will trigger autostash
        let has_dirty_changes = if is_autostash || (is_pull && !is_pull_rebase) {
            Command::new(git)
                .current_dir(&self.path)
                .args(["status", "--porcelain"])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
                .unwrap_or(false)
        } else {
            false
        };

        // Before an autostash pull, save stash attributions
        if is_autostash && has_dirty_changes {
            let _ = self.git_ai(&["stash-save"]);
        }

        // For rebase commands (including pull --rebase), capture the old commit SHAs before.
        // If the rebase has an explicit branch argument (e.g., `git rebase main feature`),
        // collect commits from that branch, not HEAD.
        let is_rebase = args.first().map(|a| *a == "rebase").unwrap_or(false);
        let is_rebase_continuation = is_rebase
            && args.iter().any(|a| *a == "--skip" || *a == "--continue");
        let pre_rebase_commits = if is_rebase || is_pull_rebase {
            if is_pull_rebase {
                // For pull --rebase, capture current HEAD's commits before the pull rewrites them
                self.collect_branch_commits()
            } else if is_rebase_continuation {
                // For --skip/--continue, read the original HEAD from rebase state
                self.read_rebase_orig_head()
                    .map(|orig| self.collect_branch_commits_from(&orig))
                    .unwrap_or_else(|| self.collect_branch_commits())
            } else {
                let rebase_branch = self.detect_rebase_branch(args);
                match rebase_branch {
                    Some(branch) => self.collect_branch_commits_from(&branch),
                    None => self.collect_branch_commits(),
                }
            }
        } else {
            Vec::new()
        };

        // Detect checkout/switch that might need working log migration
        let is_checkout_with_merge = (args.first() == Some(&"checkout") || args.first() == Some(&"switch"))
            && args.iter().any(|a| *a == "--merge" || *a == "-m");
        let pre_checkout_head = if is_checkout_with_merge {
            Command::new(git)
                .current_dir(&self.path)
                .args(["rev-parse", "HEAD"])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            None
        };

        // Detect commit --amend so we can run post-commit hook after
        let is_amend = args.first().map(|a| *a == "commit").unwrap_or(false)
            && args.iter().any(|a| *a == "--amend");

        // Before amend, capture current HEAD so we can migrate working logs
        let pre_amend_head = if is_amend {
            Command::new(git)
                .current_dir(&self.path)
                .args(["rev-parse", "HEAD"])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            None
        };

        // Detect cherry-pick commands
        let is_cherry_pick = args.first().map(|a| *a == "cherry-pick").unwrap_or(false);
        let is_cherry_pick_continuation = is_cherry_pick
            && args.iter().any(|a| *a == "--continue");
        let pre_cherry_pick_sources: Vec<String> = if is_cherry_pick {
            if is_cherry_pick_continuation {
                // For --continue, read the source from CHERRY_PICK_HEAD
                let cherry_pick_head = self.path.join(".git/CHERRY_PICK_HEAD");
                if cherry_pick_head.exists() {
                    std::fs::read_to_string(&cherry_pick_head)
                        .ok()
                        .map(|s| vec![s.trim().to_string()])
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            } else {
                // Parse the cherry-pick args to find source commit(s)
                self.resolve_cherry_pick_sources(args)
            }
        } else {
            Vec::new()
        };

        // Before stash pop/apply/branch, capture the target stash SHA
        let is_stash_restore = args.first() == Some(&"stash")
            && args.get(1).map(|s| *s == "pop" || *s == "apply" || *s == "branch").unwrap_or(false);
        let pre_stash_pop_sha = if is_stash_restore {
            // Find the explicit ref if provided (e.g., stash@{1})
            let explicit_ref = args.iter().skip(2).find(|a| a.contains("stash@{"));
            let ref_to_resolve = explicit_ref.copied().unwrap_or("stash@{0}");
            // Always resolve the stash SHA BEFORE the command runs (pop/branch drops the entry)
            Command::new(git)
                .current_dir(&self.path)
                .args(["rev-parse", ref_to_resolve])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            None
        };

        // Before reset, capture current HEAD for working log migration
        let is_reset = args.first() == Some(&"reset");
        let pre_reset_head = if is_reset {
            Command::new(git)
                .current_dir(&self.path)
                .args(["rev-parse", "HEAD"])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            None
        };

        let mut command = Command::new(git);
        command
            .current_dir(&self.path)
            .args(args)
            // Suppress git trace2 events so the system-installed git-ai daemon
            // does not detect test commits and race to generate its own notes.
            .env("GIT_TRACE2_EVENT", "/dev/null");

        let output = command
            .output()
            .unwrap_or_else(|_| panic!("Failed to execute git command: {:?}", args));

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            // After a successful pull or fetch, fetch authorship notes from the remote
            if is_pull || is_fetch {
                self.fetch_notes_from_remote(git, &pull_fetch_remote);
            }
            // After a successful rebase (or pull --rebase), copy authorship notes to new commits
            if is_rebase || is_pull_rebase {
                self.handle_post_rewrite(&pre_rebase_commits);
            }
            // After a successful cherry-pick, copy authorship notes from source to new commits
            if is_cherry_pick && !pre_cherry_pick_sources.is_empty() {
                self.handle_post_cherry_pick(&pre_cherry_pick_sources);
            }
            // After a successful amend, migrate working logs and run post-commit
            if is_amend {
                self.handle_post_amend(pre_amend_head.as_deref());
            }
            // After checkout --merge, migrate working log from old HEAD to new HEAD
            if let Some(ref old_head) = pre_checkout_head {
                let new_head = Command::new(git)
                    .current_dir(&self.path)
                    .args(["rev-parse", "HEAD"])
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
                if let Some(new_head) = new_head {
                    if new_head != *old_head {
                        self.migrate_working_log(old_head, &new_head);
                    }
                }
            }
            // After a successful push, also push notes to the remote
            if args.first() == Some(&"push") {
                let remote = self.detect_remote_from_args(args).unwrap_or_else(|| "origin".to_string());
                let _ = Command::new(git)
                    .current_dir(&self.path)
                    .args(["push", &remote, "refs/notes/ai"])
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output();
            }
            // After a successful reset, reconstruct working logs from reset commits' notes.
            // reset --soft/--mixed leave changes staged/unstaged; the next commit needs
            // the attributions from the undone commits to be available.
            if is_reset && !args.iter().any(|a| *a == "--hard") {
                if let Some(ref old_head) = pre_reset_head {
                    let new_head = Command::new(git)
                        .current_dir(&self.path)
                        .args(["rev-parse", "HEAD"])
                        .env("GIT_TRACE2_EVENT", "/dev/null")
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
                    if let Some(ref new_head) = new_head {
                        if new_head != old_head {
                            // Collect all commits between new HEAD and old HEAD
                            let commits_output = Command::new(git)
                                .current_dir(&self.path)
                                .args(["rev-list", &format!("{}..{}", new_head, old_head)])
                                .env("GIT_TRACE2_EVENT", "/dev/null")
                                .output();
                            if let Ok(co) = commits_output {
                                let reset_commits: Vec<String> = String::from_utf8_lossy(&co.stdout)
                                    .lines()
                                    .map(|s| s.trim().to_string())
                                    .filter(|s| !s.is_empty())
                                    .collect();
                                // Build INITIAL attributions from all reset commits' notes
                                self.reconstruct_working_log_from_notes(new_head, &reset_commits);
                            }
                        }
                    }
                }
            }
            // After stash push/save, save attributions
            if args.first() == Some(&"stash") {
                let sub = args.get(1).copied().unwrap_or("push");
                if sub == "push" || sub == "save" || (!sub.starts_with('-') && sub != "pop" && sub != "apply" && sub != "branch" && sub != "list" && sub != "show" && sub != "drop" && sub != "clear") {
                    let _ = self.git_ai(&["stash-save"]);
                } else if sub == "pop" || sub == "apply" || sub == "branch" {
                    if let Some(ref sha) = pre_stash_pop_sha {
                        let _ = self.git_ai(&["stash-restore-ref", sha]);
                    } else {
                        let _ = self.git_ai(&["stash-restore"]);
                    }
                }
            }
            // After a successful pull that moved HEAD, migrate working logs
            if is_pull {
                if let Some(ref old_head) = pre_pull_head {
                    let new_head = Command::new(git)
                        .current_dir(&self.path)
                        .args(["rev-parse", "HEAD"])
                        .env("GIT_TRACE2_EVENT", "/dev/null")
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
                    if let Some(ref new_head) = new_head {
                        if new_head != old_head {
                            self.migrate_working_log(old_head, new_head);
                        }
                    }
                }
                // After autostash pull completes, restore stash attributions
                if is_autostash && has_dirty_changes {
                    let _ = self.git_ai(&["stash-restore"]);
                }
            }
            Ok(if stdout.is_empty() { stderr } else { stdout })
        } else {
            // Even on failure (e.g., stash pop with conflict), try to restore
            if args.first() == Some(&"stash") {
                let sub = args.get(1).copied().unwrap_or("");
                if sub == "pop" || sub == "apply" {
                    let _ = self.git_ai(&["stash-restore"]);
                }
            }
            Err(stderr)
        }
    }

    /// Collect commit SHAs reachable from a given ref (defaults to HEAD), newest first.
    fn collect_branch_commits_from(&self, refspec: &str) -> Vec<String> {
        let git = real_git_executable();
        let output = Command::new(git)
            .current_dir(&self.path)
            .args(["log", "--format=%H", refspec])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap_or_else(|_| panic!("Failed to get commit log"));

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    fn collect_branch_commits(&self) -> Vec<String> {
        self.collect_branch_commits_from("HEAD")
    }

    /// Reconstruct working logs from authorship notes of reset/undone commits.
    /// This writes INITIAL attributions so post-commit can properly attribute lines.
    fn reconstruct_working_log_from_notes(&self, base_commit: &str, reset_commits: &[String]) {
        // Merge all notes from reset commits into a single INITIAL attribution
        let initial = self.merge_notes_into_initial(reset_commits);
        if !initial.files.is_empty() || !initial.sessions.is_empty() || !initial.humans.is_empty() || !initial.prompts.is_empty() {
            let git_dir = self.path.join(".git");
            std::fs::create_dir_all(
                git_dir.join("ai").join("working_logs").join(base_commit),
            ).ok();
            git_ai::core::working_log::write_initial_attributions(
                &git_dir,
                base_commit,
                &initial,
            );
            eprintln!(
                "[test] reconstructed working log for {} from {} reset commit(s)",
                &base_commit[..7.min(base_commit.len())],
                reset_commits.len()
            );
        }
    }

    /// Merge authorship notes from multiple commits into a single InitialAttributions.
    /// Later commits take precedence (they represent the final state).
    /// Line numbers from each commit are remapped to the current working tree state.
    fn merge_notes_into_initial(
        &self,
        commit_shas: &[String],
    ) -> git_ai::core::working_log::InitialAttributions {
        use git_ai::core::attribution::LineAttribution;
        use git_ai::core::working_log::{
            HumanRecord, InitialAttributions, PromptRecord, SessionRecord,
        };
        use std::collections::HashMap;

        let git = real_git_executable();
        let mut files: HashMap<String, Vec<LineAttribution>> = HashMap::new();
        let mut sessions: HashMap<String, SessionRecord> = HashMap::new();
        let mut humans: HashMap<String, HumanRecord> = HashMap::new();
        let mut prompts: HashMap<String, PromptRecord> = HashMap::new();

        // Process commits oldest-first (rev-list gives newest-first, so reverse)
        for commit_sha in commit_shas.iter().rev() {
            let note_content = match self.read_authorship_note(commit_sha) {
                Some(n) => n,
                None => continue,
            };
            let log = match AuthorshipLog::deserialize_from_string(&note_content) {
                Ok(l) => l,
                Err(_) => continue,
            };

            for file_att in &log.attestations {
                // Get file content at this commit for line-number remapping
                let commit_content = Command::new(git)
                    .current_dir(&self.path)
                    .args(["show", &format!("{}:{}", commit_sha, file_att.file_path)])
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string());

                // Get current working tree content
                let wt_path = self.path.join(&file_att.file_path);
                let wt_content = std::fs::read_to_string(&wt_path).ok();

                // Build line mapping from commit -> working tree
                let mapping: Option<HashMap<u32, u32>> =
                    if let (Some(old), Some(new)) = (&commit_content, &wt_content) {
                        if old != new {
                            let old_lines: Vec<&str> = old.lines().collect();
                            let new_lines: Vec<&str> = new.lines().collect();
                            Some(git_ai::core::post_commit::build_line_mapping(
                                &old_lines, &new_lines,
                            ))
                        } else {
                            None // Same content, line numbers match
                        }
                    } else {
                        None
                    };

                let mut new_line_attrs: Vec<LineAttribution> = Vec::new();
                for entry in &file_att.entries {
                    for range in &entry.line_ranges {
                        let (start, end) = match range {
                            git_ai::authorship::authorship_log::LineRange::Single(l) => (*l, *l),
                            git_ai::authorship::authorship_log::LineRange::Range(s, e) => (*s, *e),
                        };
                        for line_num in start..=end {
                            // Remap line number to working tree
                            let mapped_line = if let Some(ref m) = mapping {
                                match m.get(&line_num) {
                                    Some(&new_line) => new_line,
                                    None => continue, // Line doesn't exist in working tree
                                }
                            } else {
                                line_num
                            };

                            if let Some(last) = new_line_attrs.last_mut() {
                                if last.author_id == entry.hash
                                    && last.end_line + 1 == mapped_line
                                {
                                    last.end_line = mapped_line;
                                    continue;
                                }
                            }
                            new_line_attrs.push(LineAttribution {
                                start_line: mapped_line,
                                end_line: mapped_line,
                                author_id: entry.hash.clone(),
                                overrode: None,
                            });
                        }
                    }
                }
                if !new_line_attrs.is_empty() {
                    // Merge with existing entries: later commits override same lines,
                    // but preserve entries from earlier commits for other lines.
                    let existing = files.entry(file_att.file_path.clone()).or_default();
                    // Build a set of lines covered by the new entries
                    let mut new_lines_covered: std::collections::HashSet<u32> =
                        std::collections::HashSet::new();
                    for attr in &new_line_attrs {
                        for l in attr.start_line..=attr.end_line {
                            new_lines_covered.insert(l);
                        }
                    }
                    // Split existing ranges and remove covered lines
                    let mut split_existing: Vec<LineAttribution> = Vec::new();
                    for attr in existing.drain(..) {
                        for l in attr.start_line..=attr.end_line {
                            if !new_lines_covered.contains(&l) {
                                if let Some(last) = split_existing.last_mut() {
                                    if last.author_id == attr.author_id
                                        && last.end_line + 1 == l
                                    {
                                        last.end_line = l;
                                        continue;
                                    }
                                }
                                split_existing.push(LineAttribution {
                                    start_line: l,
                                    end_line: l,
                                    author_id: attr.author_id.clone(),
                                    overrode: None,
                                });
                            }
                        }
                    }
                    split_existing.extend(new_line_attrs);
                    split_existing.sort_by_key(|a| a.start_line);
                    *files.get_mut(&file_att.file_path).unwrap() = split_existing;
                }
            }

            for (id, session) in &log.metadata.sessions {
                sessions.insert(
                    id.clone(),
                    SessionRecord {
                        agent_id: git_ai::core::working_log::AgentId {
                            tool: session.agent_id.tool.clone(),
                            id: session.agent_id.id.clone(),
                            model: session.agent_id.model.clone(),
                        },
                        human_author: session.human_author.clone(),
                    },
                );
            }
            for (id, human) in &log.metadata.humans {
                humans.insert(
                    id.clone(),
                    HumanRecord {
                        author: human.author.clone(),
                    },
                );
            }
            for (id, prompt) in &log.metadata.prompts {
                prompts.insert(
                    id.clone(),
                    PromptRecord {
                        agent_id: git_ai::core::working_log::AgentId {
                            tool: prompt.agent_id.tool.clone(),
                            id: prompt.agent_id.id.clone(),
                            model: prompt.agent_id.model.clone(),
                        },
                        human_author: prompt.human_author.clone(),
                    },
                );
            }
        }

        InitialAttributions {
            files,
            sessions,
            humans,
            prompts,
        }
    }

    /// Fetch authorship notes from a remote repository.
    fn fetch_notes_from_remote(&self, git: &str, remote: &str) {
        let output = Command::new(git)
            .current_dir(&self.path)
            .args(["fetch", remote, "refs/notes/ai:refs/notes/ai"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output();
        match output {
            Ok(o) if o.status.success() => {
                eprintln!("[test] fetched notes from remote '{}'", remote);
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                eprintln!("[test] fetch notes from '{}' (non-fatal): {}", remote, stderr.trim());
            }
            Err(e) => {
                eprintln!("[test] fetch notes from '{}' failed (non-fatal): {}", remote, e);
            }
        }
    }

    /// Detect the remote name from pull/fetch args.
    /// Returns None if no explicit remote is found (caller should default to "origin").
    fn detect_remote_from_args(&self, args: &[&str]) -> Option<String> {
        // Skip the subcommand itself (pull/fetch) and look for a positional arg
        // that isn't a flag. For `git pull origin main` or `git fetch origin`,
        // the remote is the first non-flag argument after the subcommand.
        const VALUE_FLAGS: &[&str] = &[
            "--upload-pack", "--refmap", "--depth", "--deepen",
            "--shallow-since", "--shallow-exclude", "-o", "--server-option",
            "--negotiation-tip", "--filter", "-j", "--jobs",
            "--recurse-submodules-default", "--set-upstream",
        ];
        let mut skip_next = false;
        for arg in &args[1..] {
            if skip_next {
                skip_next = false;
                continue;
            }
            if VALUE_FLAGS.contains(arg) {
                skip_next = true;
                continue;
            }
            if arg.starts_with('-') {
                continue;
            }
            // First positional argument after the subcommand is the remote
            return Some(arg.to_string());
        }
        None
    }

    /// Check if the repo has `pull.rebase=true` configured.
    fn has_pull_rebase_config(&self) -> bool {
        let git = real_git_executable();
        Command::new(git)
            .current_dir(&self.path)
            .args(["config", "--get", "pull.rebase"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                let val = String::from_utf8_lossy(&o.stdout).trim().to_lowercase();
                val == "true" || val == "1"
            })
            .unwrap_or(false)
    }

    /// Check if the repo has `rebase.autoStash=true` configured.
    fn has_rebase_autostash_config(&self) -> bool {
        let git = real_git_executable();
        Command::new(git)
            .current_dir(&self.path)
            .args(["config", "--get", "rebase.autoStash"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                let val = String::from_utf8_lossy(&o.stdout).trim().to_lowercase();
                val == "true" || val == "1"
            })
            .unwrap_or(false)
    }

    /// Migrate working log from one base commit to another (e.g., after checkout).
    fn migrate_working_log(&self, old_head: &str, new_head: &str) {
        let git_dir = self.path.join(".git");
        let old_dir = git_dir.join("ai").join("working_logs").join(old_head);
        let new_dir = git_dir.join("ai").join("working_logs").join(new_head);
        if old_dir.exists() && !new_dir.exists() {
            let _ = std::fs::create_dir_all(new_dir.parent().unwrap());
            if let Err(e) = Self::copy_dir_recursive(&old_dir, &new_dir) {
                eprintln!("[test] migrate_working_log: failed to copy: {}", e);
            }
        }
    }

    fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let dst_path = dst.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                Self::copy_dir_recursive(&entry.path(), &dst_path)?;
            } else {
                std::fs::copy(entry.path(), dst_path)?;
            }
        }
        Ok(())
    }

    /// Read the original HEAD from an in-progress rebase state.
    fn read_rebase_orig_head(&self) -> Option<String> {
        // Try rebase-merge first (interactive rebase), then rebase-apply
        let rebase_merge = self.path.join(".git/rebase-merge/orig-head");
        let rebase_apply = self.path.join(".git/rebase-apply/orig-head");
        let path = if rebase_merge.exists() {
            rebase_merge
        } else if rebase_apply.exists() {
            rebase_apply
        } else {
            return None;
        };
        std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
    }

    /// Detect if a rebase command has an explicit branch argument.
    /// Format: `git rebase [options] [--onto <newbase>] [<upstream> [<branch>]]`
    /// Returns the branch name if present.
    fn detect_rebase_branch(&self, args: &[&str]) -> Option<String> {
        // Flags that consume the next argument as their value
        const VALUE_FLAGS: &[&str] = &["--onto", "--exec", "-x", "--strategy", "-s", "--strategy-option", "-X"];
        // Skip "rebase" itself, parse positional args (skipping flag values)
        let mut positional: Vec<&str> = Vec::new();
        let mut skip_next = false;
        for arg in &args[1..] {
            if skip_next {
                skip_next = false;
                continue;
            }
            if VALUE_FLAGS.contains(arg) {
                skip_next = true;
                continue;
            }
            if arg.starts_with('-') {
                continue;
            }
            positional.push(arg);
        }
        // Format: [upstream [branch]]
        // With --root: [branch] (no upstream needed)
        let has_root = args[1..].iter().any(|a| *a == "--root");
        if has_root && !positional.is_empty() {
            // With --root, the only positional is the branch
            Some(positional[0].to_string())
        } else if positional.len() >= 2 {
            // Without --root, second positional is the branch
            Some(positional[1].to_string())
        } else {
            None
        }
    }

    /// After an amend, build INITIAL attributions from the old commit's state
    /// (marking pre-existing lines as "human" to prevent gap-fill), migrate the
    /// working log to the parent, then run `git-ai post-commit` to generate a
    /// fresh note that correctly merges old human attribution with new AI data.
    fn handle_post_amend(&self, pre_amend_head: Option<&str>) {
        let git = real_git_executable();

        // Determine the parent of the new HEAD (what post-commit uses as base_commit)
        let new_parent = Command::new(git)
            .current_dir(&self.path)
            .args(["rev-parse", "HEAD~1"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

        let target_base = new_parent.as_deref().unwrap_or("initial");
        let git_dir = self.path.join(".git");

        if let Some(old_head) = pre_amend_head {
            // Build INITIAL attributions from the old commit's file state.
            // For each file that existed in the old commit, mark ALL lines as "human"
            // in the INITIAL file. Then overlay any AI attestations from the old note.
            // This prevents post-commit's gap-fill from incorrectly attributing
            // pre-existing human lines to AI.
            let initial = self.build_initial_from_old_commit(old_head);
            if !initial.files.is_empty()
                || !initial.sessions.is_empty()
                || !initial.humans.is_empty()
            {
                std::fs::create_dir_all(
                    git_dir
                        .join("ai")
                        .join("working_logs")
                        .join(target_base),
                )
                .ok();
                git_ai::core::working_log::write_initial_attributions(
                    &git_dir,
                    target_base,
                    &initial,
                );
            }

            // Migrate working log from old HEAD to target_base.
            // Also patch empty author_ids in checkpoints to "human" so that
            // gap-fill doesn't incorrectly attribute pre-existing lines to AI.
            if old_head != target_base {
                let old_log_dir = git_dir.join("ai").join("working_logs").join(old_head);
                let target_log_dir =
                    git_dir.join("ai").join("working_logs").join(target_base);

                if old_log_dir.exists() {
                    let old_checkpoints = old_log_dir.join("checkpoints.jsonl");
                    let target_checkpoints = target_log_dir.join("checkpoints.jsonl");

                    if old_checkpoints.exists() {
                        std::fs::create_dir_all(&target_log_dir).ok();
                        // Read and patch the checkpoints: replace empty author_id
                        // using old note to determine correct attribution
                        let patched =
                            self.patch_empty_authors_in_checkpoints(&old_checkpoints, old_head);
                        if target_checkpoints.exists() {
                            use std::io::Write;
                            if let Ok(mut f) = std::fs::OpenOptions::new()
                                .append(true)
                                .open(&target_checkpoints)
                            {
                                write!(f, "{}", patched).ok();
                            }
                        } else {
                            std::fs::write(&target_checkpoints, &patched).ok();
                        }
                    }

                    let old_blobs = old_log_dir.join("blobs");
                    let target_blobs = target_log_dir.join("blobs");
                    if old_blobs.exists() {
                        std::fs::create_dir_all(&target_blobs).ok();
                        if let Ok(entries) = std::fs::read_dir(&old_blobs) {
                            for entry in entries.flatten() {
                                let dest = target_blobs.join(entry.file_name());
                                if !dest.exists() {
                                    std::fs::copy(entry.path(), dest).ok();
                                }
                            }
                        }
                    }

                    std::fs::remove_dir_all(&old_log_dir).ok();
                }
            }
        }

        // Run post-commit which merges INITIAL + checkpoints
        let post_commit_result = self.git_ai(&["post-commit"]);
        match &post_commit_result {
            Ok(output) => eprintln!("[test] git-ai post-commit (amend) OK: {}", output),
            Err(e) => eprintln!("[test] git-ai post-commit (amend) warning: {}", e),
        }
    }

    /// Build INITIAL attributions from the old commit's authorship note,
    /// remapping line numbers through a diff to match the current working tree.
    fn build_initial_from_old_commit(
        &self,
        old_head: &str,
    ) -> git_ai::core::working_log::InitialAttributions {
        use git_ai::core::attribution::LineAttribution;
        use git_ai::core::working_log::{
            HumanRecord, InitialAttributions, PromptRecord, SessionRecord,
        };
        use std::collections::HashMap;

        let git = real_git_executable();
        let mut files: HashMap<String, Vec<LineAttribution>> = HashMap::new();
        let mut sessions: HashMap<String, SessionRecord> = HashMap::new();
        let mut humans: HashMap<String, HumanRecord> = HashMap::new();
        let mut prompts: HashMap<String, PromptRecord> = HashMap::new();

        let note_content = match self.read_authorship_note(old_head) {
            Some(n) => n,
            None => return InitialAttributions::default(),
        };
        let old_log = match AuthorshipLog::deserialize_from_string(&note_content) {
            Ok(l) => l,
            Err(_) => return InitialAttributions::default(),
        };

        for file_att in &old_log.attestations {
            let old_content = Command::new(git)
                .current_dir(&self.path)
                .args(["show", &format!("{}:{}", old_head, file_att.file_path)])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string());

            let wt_path = self.path.join(&file_att.file_path);
            let new_content = std::fs::read_to_string(&wt_path).ok();

            let (old_content, new_content) = match (old_content, new_content) {
                (Some(o), Some(n)) => (o, n),
                _ => continue,
            };

            let old_lines: Vec<&str> = old_content.lines().collect();
            let new_lines: Vec<&str> = new_content.lines().collect();
            let mapping =
                git_ai::core::post_commit::build_line_mapping(&old_lines, &new_lines);

            let mut line_attrs: Vec<LineAttribution> = Vec::new();
            for entry in &file_att.entries {
                for range in &entry.line_ranges {
                    let (start, end) = match range {
                        git_ai::authorship::authorship_log::LineRange::Single(l) => (*l, *l),
                        git_ai::authorship::authorship_log::LineRange::Range(s, e) => (*s, *e),
                    };
                    for old_line in start..=end {
                        if let Some(&new_line) = mapping.get(&old_line) {
                            if let Some(last) = line_attrs.last_mut() {
                                if last.author_id == entry.hash
                                    && last.end_line + 1 == new_line
                                {
                                    last.end_line = new_line;
                                    continue;
                                }
                            }
                            line_attrs.push(LineAttribution {
                                start_line: new_line,
                                end_line: new_line,
                                author_id: entry.hash.clone(),
                                overrode: None,
                            });
                        }
                    }
                }
            }

            if !line_attrs.is_empty() {
                files.insert(file_att.file_path.clone(), line_attrs);
            }
        }

        for (id, session) in &old_log.metadata.sessions {
            sessions.insert(
                id.clone(),
                SessionRecord {
                    agent_id: git_ai::core::working_log::AgentId {
                        tool: session.agent_id.tool.clone(),
                        id: session.agent_id.id.clone(),
                        model: session.agent_id.model.clone(),
                    },
                    human_author: session.human_author.clone(),
                },
            );
        }
        for (id, human) in &old_log.metadata.humans {
            humans.insert(
                id.clone(),
                HumanRecord {
                    author: human.author.clone(),
                },
            );
        }
        for (id, prompt) in &old_log.metadata.prompts {
            prompts.insert(
                id.clone(),
                PromptRecord {
                    agent_id: git_ai::core::working_log::AgentId {
                        tool: prompt.agent_id.tool.clone(),
                        id: prompt.agent_id.id.clone(),
                        model: prompt.agent_id.model.clone(),
                    },
                    human_author: prompt.human_author.clone(),
                },
            );
        }

        InitialAttributions {
            files,
            sessions,
            humans,
            prompts,
        }
    }

    /// Read a checkpoints.jsonl file and patch empty `author_id` fields.
    /// Uses the old commit's authorship note to determine correct attribution:
    /// - Lines that were AI in the old note keep their AI session ID
    /// - Lines that were human/unattributed get "human"
    fn patch_empty_authors_in_checkpoints(
        &self,
        path: &std::path::Path,
        old_head: &str,
    ) -> String {
        let git = real_git_executable();

        // Build mapping: file -> Vec<(old_start, old_end, session_id)> from old note
        let old_ai_lines = self.get_ai_lines_from_note(old_head);

        let content = std::fs::read_to_string(path).unwrap_or_default();
        let mut result = String::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                result.push('\n');
                continue;
            }
            if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(entries) = value.get_mut("entries").and_then(|e| e.as_array_mut()) {
                    for entry in entries.iter_mut() {
                        let file_path = entry
                            .get("file")
                            .and_then(|f| f.as_str())
                            .unwrap_or("")
                            .to_string();

                        // Build new→old line mapping for this file
                        let line_mapping = self.build_line_mapping_for_file(
                            git, old_head, &file_path,
                        );

                        let file_ai_info = old_ai_lines.get(&file_path);

                        if let Some(line_attrs) =
                            entry.get_mut("line_attributions").and_then(|a| a.as_array_mut())
                        {
                            for attr in line_attrs.iter_mut() {
                                let is_empty = attr
                                    .get("author_id")
                                    .and_then(|a| a.as_str())
                                    .map(|s| s.is_empty())
                                    .unwrap_or(false);
                                if is_empty {
                                    let start = attr
                                        .get("start_line")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0)
                                        as u32;
                                    let end = attr
                                        .get("end_line")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0)
                                        as u32;
                                    let new_author = self.find_old_author_for_lines(
                                        start,
                                        end,
                                        &line_mapping,
                                        file_ai_info,
                                    );
                                    if let Some(author) = attr.get_mut("author_id") {
                                        *author = serde_json::Value::String(new_author);
                                    }
                                }
                            }
                        }
                    }
                }
                result.push_str(
                    &serde_json::to_string(&value).unwrap_or_else(|_| line.to_string()),
                );
            } else {
                result.push_str(line);
            }
            result.push('\n');
        }
        result
    }

    /// Get AI-attributed lines from a commit's authorship note.
    fn get_ai_lines_from_note(
        &self,
        commit_sha: &str,
    ) -> std::collections::HashMap<String, Vec<(u32, u32, String)>> {
        let mut result = std::collections::HashMap::new();
        if let Some(note_content) = self.read_authorship_note(commit_sha) {
            if let Ok(log) = AuthorshipLog::deserialize_from_string(&note_content) {
                for file_att in &log.attestations {
                    let mut ranges = Vec::new();
                    for entry in &file_att.entries {
                        for range in &entry.line_ranges {
                            let (start, end) = match range {
                                git_ai::authorship::authorship_log::LineRange::Single(l) => {
                                    (*l, *l)
                                }
                                git_ai::authorship::authorship_log::LineRange::Range(s, e) => {
                                    (*s, *e)
                                }
                            };
                            ranges.push((start, end, entry.hash.clone()));
                        }
                    }
                    if !ranges.is_empty() {
                        result.insert(file_att.file_path.clone(), ranges);
                    }
                }
            }
        }
        result
    }

    /// Build a mapping from new line numbers to old line numbers for a file.
    fn build_line_mapping_for_file(
        &self,
        git: &str,
        old_head: &str,
        file_path: &str,
    ) -> std::collections::HashMap<u32, u32> {
        let old_content = Command::new(git)
            .current_dir(&self.path)
            .args(["show", &format!("{}:{}", old_head, file_path)])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string());

        let new_content = self.read_file(file_path);

        match (old_content, new_content) {
            (Some(old), Some(new)) => {
                let old_lines: Vec<&str> = old.lines().collect();
                let new_lines: Vec<&str> = new.lines().collect();
                self.lcs_line_mapping(&old_lines, &new_lines)
            }
            _ => std::collections::HashMap::new(),
        }
    }

    /// Determine the correct author for lines that have empty attribution.
    /// Maps new line numbers back to old line numbers and checks old AI attribution.
    fn find_old_author_for_lines(
        &self,
        start: u32,
        end: u32,
        line_mapping: &std::collections::HashMap<u32, u32>,
        ai_info: Option<&Vec<(u32, u32, String)>>,
    ) -> String {
        if let Some(ai_ranges) = ai_info {
            for new_line in start..=end {
                if let Some(&old_line) = line_mapping.get(&new_line) {
                    for &(ai_start, ai_end, ref session_id) in ai_ranges {
                        if old_line >= ai_start && old_line <= ai_end {
                            return session_id.clone();
                        }
                    }
                }
            }
        }
        "human".to_string()
    }

    /// Build a mapping from new line numbers to old line numbers using LCS.
    fn lcs_line_mapping(
        &self,
        old_lines: &[&str],
        new_lines: &[&str],
    ) -> std::collections::HashMap<u32, u32> {
        let mut mapping = std::collections::HashMap::new();
        let n = old_lines.len();
        let m = new_lines.len();

        if n == 0 || m == 0 {
            return mapping;
        }

        // Build LCS table
        let mut dp = vec![vec![0u32; m + 1]; n + 1];
        for i in 1..=n {
            for j in 1..=m {
                if old_lines[i - 1] == new_lines[j - 1] {
                    dp[i][j] = dp[i - 1][j - 1] + 1;
                } else {
                    dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
                }
            }
        }

        // Backtrack to find the mapping
        let mut i = n;
        let mut j = m;
        while i > 0 && j > 0 {
            if old_lines[i - 1] == new_lines[j - 1] {
                mapping.insert(j as u32, i as u32);
                i -= 1;
                j -= 1;
            } else if dp[i - 1][j] > dp[i][j - 1] {
                i -= 1;
            } else {
                j -= 1;
            }
        }

        mapping
    }

    /// After a rebase, find old->new commit mappings and copy authorship notes.
    fn handle_post_rewrite(&self, pre_rebase_commits: &[String]) {
        let git = real_git_executable();
        let pre_rebase_set: std::collections::HashSet<&String> =
            pre_rebase_commits.iter().collect();

        // Get all commits that have authorship notes
        let notes_output = Command::new(git)
            .current_dir(&self.path)
            .args(["notes", "--ref=ai", "list"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output();

        let noted_commits: Vec<String> = match notes_output {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(|line| {
                        // Format: "<note-blob-sha> <commit-sha>"
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        if parts.len() >= 2 {
                            Some(parts[1].to_string())
                        } else {
                            None
                        }
                    })
                    .collect()
            }
            _ => return,
        };

        // Get the current set of commits on this branch (HEAD)
        let post_rebase_commits: Vec<String> = {
            let output = Command::new(git)
                .current_dir(&self.path)
                .args(["log", "--format=%H", "HEAD"])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };

        // Find old commits that had notes and were on the branch before rebase
        // but are no longer on the branch after rebase (they were rewritten)
        let post_set: std::collections::HashSet<&String> =
            post_rebase_commits.iter().collect();
        let noted_set: std::collections::HashSet<&String> =
            noted_commits.iter().collect();
        let old_with_notes: Vec<&String> = pre_rebase_commits
            .iter()
            .filter(|c| noted_set.contains(c) && !post_set.contains(c))
            .collect();

        eprintln!("[test] post-rewrite: noted_commits={:?}", noted_commits.iter().map(|s| &s[..7]).collect::<Vec<_>>());
        eprintln!("[test] post-rewrite: post_rebase_commits={:?}", post_rebase_commits.iter().map(|s| &s[..7]).collect::<Vec<_>>());
        eprintln!("[test] post-rewrite: old_with_notes={:?}", old_with_notes.iter().map(|s| &s[..7]).collect::<Vec<_>>());

        if old_with_notes.is_empty() {
            eprintln!("[test] post-rewrite: no old commits with notes found");
            return;
        }

        // Build a map of old_sha -> subject for all old commits with notes
        let mut old_msgs: Vec<(String, String)> = Vec::new(); // (sha, subject)
        for old_sha in &old_with_notes {
            let old_msg = match Command::new(git)
                .current_dir(&self.path)
                .args(["log", "-1", "--format=%s", old_sha.as_str()])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
            {
                Ok(o) if o.status.success() => {
                    String::from_utf8_lossy(&o.stdout).trim().to_string()
                }
                _ => continue,
            };
            old_msgs.push((old_sha.to_string(), old_msg));
        }

        // Build map of new commit subjects — only genuinely new commits (not pre-existing)
        let mut new_msgs: Vec<(String, String)> = Vec::new();
        // Also collect subjects of new commits that ALREADY have notes (e.g., from prior rebases)
        let mut already_noted_new_msgs: Vec<(String, String)> = Vec::new();
        for new_c in &post_rebase_commits {
            if pre_rebase_set.contains(new_c) {
                continue;
            }
            let msg = Command::new(git)
                .current_dir(&self.path)
                .args(["log", "-1", "--format=%s", new_c.as_str()])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            if noted_commits.contains(new_c) {
                already_noted_new_msgs.push((new_c.clone(), msg));
            } else {
                new_msgs.push((new_c.clone(), msg));
            }
        }

        // Filter out old commits that already have a matching new commit with a note
        // (these were handled by a previous rebase in the same stack)
        let old_msgs: Vec<(String, String)> = old_msgs.into_iter().filter(|(_, old_msg)| {
            !already_noted_new_msgs.iter().any(|(_, new_msg)| new_msg == old_msg)
        }).collect();

        if old_msgs.is_empty() {
            eprintln!("[test] post-rewrite: all old commits already have matching noted new commits");
            return;
        }

        // First pass: find 1:1 matches by subject
        let mut matched_old: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut matched_new: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut one_to_one: Vec<(String, String)> = Vec::new(); // (old, new)

        for (old_sha, old_msg) in &old_msgs {
            if let Some((new_sha, _)) = new_msgs.iter().find(|(ns, nm)| {
                nm == old_msg && !matched_new.contains(ns)
            }) {
                matched_old.insert(old_sha.clone());
                matched_new.insert(new_sha.clone());
                one_to_one.push((old_sha.clone(), new_sha.clone()));
            }
        }

        // Collect unmatched old commits
        let unmatched_old: Vec<String> = old_msgs
            .iter()
            .filter(|(sha, _)| !matched_old.contains(sha))
            .map(|(sha, _)| sha.clone())
            .collect();

        // Collect unmatched new commits (new commits that don't have notes and weren't matched)
        let unmatched_new: Vec<String> = new_msgs
            .iter()
            .filter(|(ns, _)| !matched_new.contains(ns) && !noted_commits.contains(ns))
            .map(|(ns, _)| ns.clone())
            .collect();

        if !unmatched_old.is_empty() && unmatched_old.len() > unmatched_new.len() {
            // More unmatched old than new → potential squash scenario.
            // But first verify: the unmatched old commits' files must actually be present
            // in the candidate squash target. If they're not, the commits were DROPPED (skip),
            // not squashed.
            let candidate_target = if one_to_one.len() == 1 && unmatched_new.is_empty() {
                Some(one_to_one[0].1.clone())
            } else if unmatched_new.len() == 1 {
                Some(unmatched_new[0].clone())
            } else {
                None
            };

            // Verify squash by checking if the target commit's tree contains the
            // unmatched old commits' changed files.
            let squash_target = candidate_target.and_then(|target| {
                // Get files changed in the target commit
                let target_files: std::collections::HashSet<String> = Command::new(git)
                    .current_dir(&self.path)
                    .args(["diff-tree", "--no-commit-id", "-r", "--name-only", &target])
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| {
                        String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();

                // Check if ANY unmatched old commit's files are in the target
                let has_squashed_content = unmatched_old.iter().any(|old_sha| {
                    let old_files: Vec<String> = Command::new(git)
                        .current_dir(&self.path)
                        .args(["diff-tree", "--no-commit-id", "-r", "--name-only", old_sha])
                        .env("GIT_TRACE2_EVENT", "/dev/null")
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| {
                            String::from_utf8_lossy(&o.stdout)
                                .lines()
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect()
                        })
                        .unwrap_or_default();
                    old_files.iter().any(|f| target_files.contains(f))
                });

                if has_squashed_content {
                    Some(target)
                } else {
                    // Files from unmatched old commits are NOT in the target — they were dropped/skipped
                    eprintln!("[test] post-rewrite: unmatched old commits' files not in target, treating as drop (not squash)");
                    None
                }
            });

            if let Some(target) = squash_target {
                // Reverse to chronological order (oldest first) since pre_rebase_commits is newest-first
                let all_originals: Vec<String> = old_msgs.iter().rev().map(|(sha, _)| sha.clone()).collect();
                eprintln!(
                    "[test] post-rewrite: squash transfer {:?} -> {}",
                    all_originals.iter().map(|s| &s[..7]).collect::<Vec<_>>(),
                    &target[..7]
                );
                let binary_path = get_binary_path();
                let mut args = vec!["post-rewrite-squash".to_string(), target.clone()];
                args.extend(all_originals);
                let output = Command::new(binary_path)
                    .current_dir(&self.path)
                    .args(&args)
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output();
                if let Ok(o) = &output {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    if !stdout.is_empty() || !stderr.is_empty() {
                        eprintln!("[test] post-rewrite-squash result: {} {}", stdout.trim(), stderr.trim());
                    }
                }

                // Also transfer any remaining 1:1 matches that weren't part of the squash
                for (old_sha, new_sha) in &one_to_one {
                    if *new_sha == target {
                        continue;
                    }
                    eprintln!("[test] post-rewrite: transferring {} -> {}", &old_sha[..7], &new_sha[..7]);
                    let binary_path = get_binary_path();
                    let output = Command::new(binary_path)
                        .current_dir(&self.path)
                        .args(["post-rewrite", old_sha, new_sha])
                        .env("GIT_TRACE2_EVENT", "/dev/null")
                        .output();
                    if let Ok(o) = &output {
                        let stdout = String::from_utf8_lossy(&o.stdout);
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        if !stdout.is_empty() || !stderr.is_empty() {
                            eprintln!("[test] post-rewrite result: {} {}", stdout.trim(), stderr.trim());
                        }
                    }
                }
            } else {
                // No squash target found, fall back to 1:1 transfers only
                for (old_sha, new_sha) in &one_to_one {
                    eprintln!("[test] post-rewrite: transferring {} -> {}", &old_sha[..7], &new_sha[..7]);
                    let binary_path = get_binary_path();
                    let output = Command::new(binary_path)
                        .current_dir(&self.path)
                        .args(["post-rewrite", old_sha, new_sha])
                        .env("GIT_TRACE2_EVENT", "/dev/null")
                        .output();
                    if let Ok(o) = &output {
                        let stdout = String::from_utf8_lossy(&o.stdout);
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        if !stdout.is_empty() || !stderr.is_empty() {
                            eprintln!("[test] post-rewrite result: {} {}", stdout.trim(), stderr.trim());
                        }
                    }
                }
            }
        } else if !unmatched_old.is_empty() {
            // Equal number of unmatched old and new → reword/patch-id matching.
            // Try to match unmatched old to unmatched new by tree (diff) content.
            let mut remaining_new: Vec<String> = unmatched_new.clone();
            for old_sha in &unmatched_old {
                // Try patch-id matching
                let old_patch_id = Command::new(git)
                    .current_dir(&self.path)
                    .args(["diff-tree", "-p", old_sha.as_str()])
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output()
                    .ok()
                    .and_then(|diff_out| {
                        let mut child = Command::new(git)
                            .current_dir(&self.path)
                            .args(["patch-id", "--stable"])
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::piped())
                            .env("GIT_TRACE2_EVENT", "/dev/null")
                            .spawn()
                            .ok()?;
                        use std::io::Write;
                        child.stdin.take()?.write_all(&diff_out.stdout).ok()?;
                        let output = child.wait_with_output().ok()?;
                        let line = String::from_utf8_lossy(&output.stdout);
                        line.split_whitespace().next().map(|s| s.to_string())
                    });

                let matched_new_sha = if let Some(ref old_pid) = old_patch_id {
                    remaining_new.iter().position(|new_c| {
                        let new_pid = Command::new(git)
                            .current_dir(&self.path)
                            .args(["diff-tree", "-p", new_c.as_str()])
                            .env("GIT_TRACE2_EVENT", "/dev/null")
                            .output()
                            .ok()
                            .and_then(|diff_out| {
                                let mut child = Command::new(git)
                                    .current_dir(&self.path)
                                    .args(["patch-id", "--stable"])
                                    .stdin(std::process::Stdio::piped())
                                    .stdout(std::process::Stdio::piped())
                                    .env("GIT_TRACE2_EVENT", "/dev/null")
                                    .spawn()
                                    .ok()?;
                                use std::io::Write;
                                child.stdin.take()?.write_all(&diff_out.stdout).ok()?;
                                let output = child.wait_with_output().ok()?;
                                let line = String::from_utf8_lossy(&output.stdout);
                                line.split_whitespace().next().map(|s| s.to_string())
                            });
                        new_pid.as_deref() == Some(old_pid.as_str())
                    })
                } else {
                    None
                };

                if let Some(idx) = matched_new_sha {
                    let new_sha = remaining_new.remove(idx);
                    eprintln!("[test] post-rewrite: transferring {} -> {} (patch-id match)", &old_sha[..7], &new_sha[..7]);
                    let binary_path = get_binary_path();
                    let output = Command::new(binary_path)
                        .current_dir(&self.path)
                        .args(["post-rewrite", old_sha, &new_sha])
                        .env("GIT_TRACE2_EVENT", "/dev/null")
                        .output();
                    if let Ok(o) = &output {
                        let stdout = String::from_utf8_lossy(&o.stdout);
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        if !stdout.is_empty() || !stderr.is_empty() {
                            eprintln!("[test] post-rewrite result: {} {}", stdout.trim(), stderr.trim());
                        }
                    }
                } else {
                    eprintln!("[test] post-rewrite: no patch-id match for old {}", &old_sha[..7]);
                }
            }

            // Transfer all subject-matched pairs
            for (old_sha, new_sha) in &one_to_one {
                eprintln!("[test] post-rewrite: transferring {} -> {}", &old_sha[..7], &new_sha[..7]);
                let binary_path = get_binary_path();
                let output = Command::new(binary_path)
                    .current_dir(&self.path)
                    .args(["post-rewrite", old_sha, new_sha])
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output();
                if let Ok(o) = &output {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    if !stdout.is_empty() || !stderr.is_empty() {
                        eprintln!("[test] post-rewrite result: {} {}", stdout.trim(), stderr.trim());
                    }
                }
            }
        } else {
            // No unmatched old commits — pure 1:1 rebase scenario
            for (old_sha, new_sha) in &one_to_one {
                eprintln!("[test] post-rewrite: transferring {} -> {}", &old_sha[..7], &new_sha[..7]);
                let binary_path = get_binary_path();
                let output = Command::new(binary_path)
                    .current_dir(&self.path)
                    .args(["post-rewrite", old_sha, new_sha])
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output();
                if let Ok(o) = &output {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    if !stdout.is_empty() || !stderr.is_empty() {
                        eprintln!("[test] post-rewrite result: {} {}", stdout.trim(), stderr.trim());
                    }
                }
            }
        }
    }

    /// Resolve the source commit SHAs for a cherry-pick command.
    /// Handles single commits, multiple commits, and ranges (A..B).
    fn resolve_cherry_pick_sources(&self, args: &[&str]) -> Vec<String> {
        let git = real_git_executable();
        // Flags that consume the next argument as their value
        const VALUE_FLAGS: &[&str] = &[
            "--strategy", "-s", "--strategy-option", "-X", "--cleanup",
            "--mainline", "-m",
        ];
        let mut sources: Vec<String> = Vec::new();
        let mut skip_next = false;

        for arg in &args[1..] {
            if skip_next {
                skip_next = false;
                continue;
            }
            if VALUE_FLAGS.contains(arg) {
                skip_next = true;
                continue;
            }
            if arg.starts_with('-') {
                continue;
            }
            // This is a positional argument — could be a single ref or a range
            let arg_str = *arg;
            if arg_str.contains("..") {
                // Range: resolve all commits in the range (oldest first)
                let output = Command::new(git)
                    .current_dir(&self.path)
                    .args(["rev-list", "--reverse", arg_str])
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output();
                if let Ok(o) = output {
                    if o.status.success() {
                        for line in String::from_utf8_lossy(&o.stdout).lines() {
                            let sha = line.trim().to_string();
                            if !sha.is_empty() {
                                sources.push(sha);
                            }
                        }
                    }
                }
            } else {
                // Single ref: resolve to SHA
                let output = Command::new(git)
                    .current_dir(&self.path)
                    .args(["rev-parse", arg_str])
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output();
                if let Ok(o) = output {
                    if o.status.success() {
                        let sha = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        if !sha.is_empty() {
                            sources.push(sha);
                        }
                    }
                }
            }
        }
        sources
    }

    /// After a cherry-pick, call `git-ai post-rewrite` for each source→new commit pair.
    /// For a single cherry-pick, new commit is HEAD.
    /// For a range cherry-pick with N commits, the new commits are HEAD~(N-1)..HEAD.
    fn handle_post_cherry_pick(&self, source_shas: &[String]) {
        let git = real_git_executable();
        let n = source_shas.len();

        // Collect the new commit SHAs (HEAD~(n-1), HEAD~(n-2), ..., HEAD)
        let new_shas: Vec<String> = (0..n)
            .rev()
            .filter_map(|i| {
                let rev = if i == 0 {
                    "HEAD".to_string()
                } else {
                    format!("HEAD~{}", i)
                };
                Command::new(git)
                    .current_dir(&self.path)
                    .args(["rev-parse", &rev])
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            })
            .collect();

        if new_shas.len() != n {
            eprintln!(
                "[test] post-cherry-pick: expected {} new commits but resolved {}",
                n,
                new_shas.len()
            );
            return;
        }

        for (old_sha, new_sha) in source_shas.iter().zip(new_shas.iter()) {
            eprintln!(
                "[test] post-cherry-pick: transferring {} -> {}",
                &old_sha[..std::cmp::min(7, old_sha.len())],
                &new_sha[..std::cmp::min(7, new_sha.len())]
            );
            let binary_path = get_binary_path();
            let output = Command::new(binary_path)
                .current_dir(&self.path)
                .args(["post-rewrite", old_sha, new_sha])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output();
            if let Ok(o) = &output {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if !stdout.is_empty() || !stderr.is_empty() {
                    eprintln!(
                        "[test] post-cherry-pick result: {} {}",
                        stdout.trim(),
                        stderr.trim()
                    );
                }
            }
        }
    }

    /// Run a git-ai subcommand.
    pub fn git_ai(&self, args: &[&str]) -> Result<String, String> {
        let binary_path = get_binary_path();
        let mut command = Command::new(binary_path);
        command
            .args(args)
            .current_dir(&self.path)
            // Suppress git trace2 events so the system daemon doesn't interfere.
            .env("GIT_TRACE2_EVENT", "/dev/null");

        // Pass additional daemon env vars (from new_with_daemon_env).
        for (key, value) in &self.daemon_env {
            command.env(key, value);
        }

        // Pass config patch as env var.
        if let Some(ref patch) = self.config_patch {
            if let Ok(json) = serde_json::to_string(patch) {
                command.env("GIT_AI_TEST_CONFIG_PATCH", json);
            }
        }

        // Set HOME to test_home_path for isolation.
        command.env("HOME", self._tempdir.path());

        let output = command
            .output()
            .unwrap_or_else(|_| panic!("Failed to execute git-ai command: {:?}", args));

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            let combined = if stdout.is_empty() {
                stderr
            } else if stderr.is_empty() {
                stdout
            } else {
                format!("{}{}", stdout, stderr)
            };
            Ok(combined)
        } else {
            let combined = if stdout.is_empty() {
                stderr
            } else if stderr.is_empty() {
                stdout
            } else {
                format!("{}{}", stderr, stdout)
            };
            Err(combined)
        }
    }

    /// Alias kept for compatibility with tests that call git_og.
    pub fn git_og(&self, args: &[&str]) -> Result<String, String> {
        self.git(args)
    }

    /// Stage all files and commit, then run post-commit to generate authorship.
    pub fn stage_all_and_commit(&self, message: &str) -> Result<NewCommit, String> {
        self.git(&["add", "-A"]).expect("add --all should succeed");
        self.commit(message)
    }

    /// Commit (no add), then run git-ai post-commit to generate the authorship note.
    pub fn commit(&self, message: &str) -> Result<NewCommit, String> {
        let output = self.git(&["commit", "-m", message]);

        match output {
            Ok(stdout) => {
                // Run git-ai post-commit to generate authorship note
                let post_commit_result = self.git_ai(&["post-commit"]);
                match &post_commit_result {
                    Ok(output) => eprintln!("[test] git-ai post-commit OK: {}", output),
                    Err(e) => eprintln!("[test] git-ai post-commit warning: {}", e),
                }

                // Get HEAD commit SHA
                let head_sha = self
                    .git(&["rev-parse", "HEAD"])
                    .map_err(|e| format!("Failed to get HEAD: {}", e))?
                    .trim()
                    .to_string();

                // Read the authorship note
                let note_content = self.read_authorship_note(&head_sha).ok_or_else(|| {
                    format!("No authorship log found for commit {}", &head_sha[..7])
                })?;

                let authorship_log = AuthorshipLog::deserialize_from_string(&note_content)
                    .map_err(|e| format!("Failed to parse authorship log: {}", e))?;

                Ok(NewCommit {
                    commit_sha: head_sha,
                    authorship_log,
                    stdout,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Create a TestFile handle for the given filename.
    pub fn filename(&self, filename: &str) -> TestFile<'_> {
        let file_path = self.path.join(filename);

        if file_path.exists() {
            TestFile::from_existing_file(file_path, self)
        } else {
            TestFile::new_with_filename(file_path, vec![], self)
        }
    }

    /// Read the authorship note for a given commit SHA.
    pub fn read_authorship_note(&self, commit_sha: &str) -> Option<String> {
        self.git(&["notes", "--ref=ai", "show", commit_sha])
            .ok()
            .filter(|note| !note.trim().is_empty())
    }

    /// Read a file from the working directory (relative to repo root).
    /// Returns None if the file doesn't exist.
    pub fn read_file(&self, relative_path: &str) -> Option<String> {
        let file_path = self.path.join(relative_path);
        std::fs::read_to_string(file_path).ok()
    }

    /// Run a real git command with additional environment variables.
    /// `env_vars` is a slice of (key, value) pairs.
    /// `stdin_content` is optional content to pipe to stdin.
    pub fn git_with_env(
        &self,
        args: &[&str],
        env_vars: &[(&str, &str)],
        stdin_content: Option<&str>,
    ) -> Result<String, String> {
        let git = real_git_executable();

        let is_rebase = args.first().map(|a| *a == "rebase").unwrap_or(false);
        let is_rebase_continuation = is_rebase
            && args.iter().any(|a| *a == "--skip" || *a == "--continue");
        let pre_rebase_commits = if is_rebase {
            if is_rebase_continuation {
                // For --skip/--continue, read the original HEAD from rebase state
                self.read_rebase_orig_head()
                    .map(|orig| self.collect_branch_commits_from(&orig))
                    .unwrap_or_else(|| self.collect_branch_commits())
            } else {
                let rebase_branch = self.detect_rebase_branch(args);
                match rebase_branch {
                    Some(branch) => self.collect_branch_commits_from(&branch),
                    None => self.collect_branch_commits(),
                }
            }
        } else {
            Vec::new()
        };

        let mut command = Command::new(git);
        command
            .current_dir(&self.path)
            .args(args)
            .env("GIT_TRACE2_EVENT", "/dev/null");

        for (key, value) in env_vars {
            command.env(key, value);
        }

        let result = if let Some(input) = stdin_content {
            use std::io::Write;
            command.stdin(std::process::Stdio::piped());
            let mut child = command
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap_or_else(|_| panic!("Failed to execute git command: {:?}", args));
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(input.as_bytes()).unwrap();
            }
            let output = child.wait_with_output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if output.status.success() {
                Ok(if stdout.is_empty() { stderr } else { stdout })
            } else {
                Err(stderr)
            }
        } else {
            let output = command
                .output()
                .unwrap_or_else(|_| panic!("Failed to execute git command: {:?}", args));
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if output.status.success() {
                Ok(if stdout.is_empty() { stderr } else { stdout })
            } else {
                Err(stderr)
            }
        };

        if is_rebase && result.is_ok() {
            self.handle_post_rewrite(&pre_rebase_commits);
        }

        result
    }

    /// Get the current branch name.
    pub fn current_branch(&self) -> String {
        self.git(&["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap_or_else(|_| "main".to_string())
            .trim()
            .to_string()
    }

    /// Returns the canonicalized path to the test repo.
    pub fn canonical_path(&self) -> PathBuf {
        self.path
            .canonicalize()
            .expect("failed to canonicalize test repo path")
    }

    /// Create a pair of repos: (local, remote) where local has remote set as origin.
    pub fn new_with_remote() -> (Self, Self) {
        let git = real_git_executable();

        // Create bare upstream
        let upstream_dir = tempfile::tempdir().expect("failed to create upstream temp dir");
        let upstream_path = upstream_dir.path().to_path_buf();
        let output = Command::new(git)
            .args(["init", "--bare", upstream_path.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .expect("failed to init bare repo");
        assert!(output.status.success(), "git init --bare failed");

        // Set default branch to main
        Command::new(git)
            .args(["-C", upstream_path.to_str().unwrap(), "symbolic-ref", "HEAD", "refs/heads/main"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .ok();

        let upstream = TestRepo {
            path: upstream_path.clone(),
            _tempdir: upstream_dir,
            daemon_env: Vec::new(),
            config_patch: None,
        };

        // Create local clone
        let local_dir = tempfile::tempdir().expect("failed to create local temp dir");
        let local_path = local_dir.path().to_path_buf();
        let output = Command::new(git)
            .args(["clone", upstream_path.to_str().unwrap(), local_path.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .expect("failed to clone");
        assert!(output.status.success(), "git clone failed: {}", String::from_utf8_lossy(&output.stderr));

        // Configure local
        for args in [
            vec!["-C", local_path.to_str().unwrap(), "config", "user.name", "Test User"],
            vec!["-C", local_path.to_str().unwrap(), "config", "user.email", "test@example.com"],
        ] {
            Command::new(git)
                .args(&args)
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .ok();
        }

        let local = TestRepo {
            path: local_path,
            _tempdir: local_dir,
            daemon_env: Vec::new(),
            config_patch: None,
        };

        (local, upstream)
    }

    /// Create a TestRepo at a specific path (the caller manages the directory lifetime).
    pub fn new_at_path(path: &Path) -> Self {
        let git = real_git_executable();
        let p = path.to_str().unwrap();

        std::fs::create_dir_all(path).expect("failed to create path");

        let output = Command::new(git)
            .args(["init", p])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .expect("failed to init repo at path");
        assert!(output.status.success(), "git init failed at {}", p);

        for args in [
            vec!["-C", p, "config", "user.name", "Test User"],
            vec!["-C", p, "config", "user.email", "test@example.com"],
            vec!["-C", p, "symbolic-ref", "HEAD", "refs/heads/main"],
        ] {
            Command::new(git)
                .args(&args)
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .ok();
        }

        // Use a dummy tempdir since we don't own the path
        let tempdir = tempfile::tempdir().expect("failed to create temp dir");
        TestRepo {
            path: path.to_path_buf(),
            _tempdir: tempdir,
            daemon_env: Vec::new(),
            config_patch: None,
        }
    }

    /// Run git command with environment variables (no stdin support).
    pub fn git_og_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String> {
        let git = real_git_executable();
        let mut command = Command::new(git);
        command
            .current_dir(&self.path)
            .args(args)
            .env("GIT_TRACE2_EVENT", "/dev/null");
        for (key, value) in envs {
            command.env(key, value);
        }
        let output = command
            .output()
            .unwrap_or_else(|_| panic!("Failed to execute git command: {:?}", args));
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if output.status.success() {
            Ok(if stdout.is_empty() { stderr } else { stdout })
        } else {
            Err(stderr)
        }
    }

    /// Run git-ai from a specific working directory (for subdir tests).
    pub fn git_ai_from_working_dir(
        &self,
        working_dir: &std::path::Path,
        args: &[&str],
    ) -> Result<String, String> {
        let binary_path = get_binary_path();
        let mut command = Command::new(binary_path);
        command
            .args(args)
            .current_dir(working_dir)
            .env("GIT_TRACE2_EVENT", "/dev/null");
        let output = command
            .output()
            .unwrap_or_else(|_| panic!("Failed to execute git-ai command: {:?}", args));
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if output.status.success() {
            let combined = if stdout.is_empty() { stderr } else if stderr.is_empty() { stdout } else { format!("{}{}", stdout, stderr) };
            Ok(combined)
        } else {
            let combined = if stdout.is_empty() { stderr } else if stderr.is_empty() { stdout } else { format!("{}{}", stderr, stdout) };
            Err(combined)
        }
    }

    /// Run git-ai with additional environment variables.
    pub fn git_ai_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String> {
        let binary_path = get_binary_path();
        let mut command = Command::new(binary_path);
        command
            .args(args)
            .current_dir(&self.path)
            .env("GIT_TRACE2_EVENT", "/dev/null");
        for (key, value) in envs {
            command.env(key, value);
        }
        let output = command
            .output()
            .unwrap_or_else(|_| panic!("Failed to execute git-ai command: {:?}", args));
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if output.status.success() {
            let combined = if stdout.is_empty() { stderr } else if stderr.is_empty() { stdout } else { format!("{}{}", stdout, stderr) };
            Ok(combined)
        } else {
            let combined = if stdout.is_empty() { stderr } else if stderr.is_empty() { stdout } else { format!("{}{}", stderr, stdout) };
            Err(combined)
        }
    }

    /// Run git-ai with stdin input.
    pub fn git_ai_with_stdin(&self, args: &[&str], stdin_data: &[u8]) -> Result<String, String> {
        use std::io::Write;
        let binary_path = get_binary_path();
        let mut child = Command::new(binary_path)
            .args(args)
            .current_dir(&self.path)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap_or_else(|_| panic!("Failed to spawn git-ai command: {:?}", args));
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_data).ok();
        }
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if output.status.success() {
            let combined = if stdout.is_empty() { stderr } else if stderr.is_empty() { stdout } else { format!("{}{}", stdout, stderr) };
            Ok(combined)
        } else {
            let combined = if stdout.is_empty() { stderr } else if stderr.is_empty() { stdout } else { format!("{}{}", stderr, stdout) };
            Err(combined)
        }
    }
}
