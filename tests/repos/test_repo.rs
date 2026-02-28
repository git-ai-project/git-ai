#![allow(dead_code)]

use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::authorship::stats::CommitStats;
use git_ai::config::ConfigPatch;
use git_ai::feature_flags::FeatureFlags;
use git_ai::git::repo_storage::PersistedWorkingLog;
use git_ai::git::repository as GitAiRepository;
use git_ai::observability::wrapper_performance_targets::BenchmarkResult;
use git2::Repository;
use insta::{Settings, assert_debug_snapshot};
use rand::Rng;
use std::cell::Cell;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use super::test_file::TestFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitTestMode {
    Wrapper,
    Hooks,
    Both,
}

impl GitTestMode {
    fn from_env() -> Self {
        let mode = std::env::var("GIT_AI_TEST_GIT_MODE")
            .unwrap_or_else(|_| "wrapper".to_string())
            .to_lowercase();
        Self::from_mode_name(&mode)
    }

    pub fn from_mode_name(mode: &str) -> Self {
        match mode.to_lowercase().as_str() {
            "hooks" => Self::Hooks,
            "both" | "wrapper+hooks" | "hooks+wrapper" => Self::Both,
            _ => Self::Wrapper,
        }
    }

    fn uses_wrapper(self) -> bool {
        matches!(self, Self::Wrapper | Self::Both)
    }

    fn uses_hooks(self) -> bool {
        matches!(self, Self::Hooks | Self::Both)
    }
}

thread_local! {
    static WORKTREE_MODE: Cell<bool> = const { Cell::new(false) };
}

pub fn with_worktree_mode<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    WORKTREE_MODE.with(|flag| {
        let previous = flag.replace(true);

        struct Reset<'a> {
            flag: &'a Cell<bool>,
            previous: bool,
        }
        impl<'a> Drop for Reset<'a> {
            fn drop(&mut self) {
                self.flag.set(self.previous);
            }
        }
        let _reset = Reset { flag, previous };

        let mut settings = Settings::clone_current();
        settings.set_snapshot_suffix("worktree");
        settings.bind(f)
    })
}

#[cfg(unix)]
fn create_file_symlink(target: &PathBuf, link: &PathBuf) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_file_symlink(target: &PathBuf, link: &PathBuf) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
        .or_else(|_| std::fs::copy(target, link).map(|_| ()))
}

fn resolve_test_db_path(
    base: &std::path::Path,
    id: u64,
    test_home: &std::path::Path,
    git_mode: GitTestMode,
) -> PathBuf {
    if git_mode.uses_hooks() {
        test_home.join(".git-ai").join("internal").join("db")
    } else {
        base.join(format!("{}-db", id))
    }
}

#[derive(Clone, Debug)]
pub struct TestRepo {
    path: PathBuf,
    pub feature_flags: FeatureFlags,
    pub(crate) config_patch: Option<ConfigPatch>,
    test_db_path: PathBuf,
    test_home: PathBuf,
    git_mode: GitTestMode,
    /// When this TestRepo is backed by a linked worktree, holds the base repo path
    /// so we can clean it up on drop.
    _base_repo_path: Option<PathBuf>,
    /// Base repo's test DB path for cleanup.
    _base_test_db_path: Option<PathBuf>,
}

#[allow(dead_code)]
impl Default for TestRepo {
    fn default() -> Self {
        Self::new()
    }
}

impl TestRepo {
    fn sync_test_home_config_for_hooks(&self) {
        if !self.git_mode.uses_hooks() {
            return;
        }

        let Some(patch) = &self.config_patch else {
            return;
        };

        let mut config = serde_json::Map::new();

        if let Some(exclude) = &patch.exclude_prompts_in_repositories {
            let values = exclude
                .iter()
                .map(|pattern| serde_json::Value::String(pattern.clone()))
                .collect();
            config.insert(
                "exclude_prompts_in_repositories".to_string(),
                serde_json::Value::Array(values),
            );
        }
        if let Some(telemetry_oss_disabled) = patch.telemetry_oss_disabled {
            let value = if telemetry_oss_disabled { "off" } else { "on" };
            config.insert(
                "telemetry_oss".to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
        if let Some(disable_version_checks) = patch.disable_version_checks {
            config.insert(
                "disable_version_checks".to_string(),
                serde_json::Value::Bool(disable_version_checks),
            );
        }
        if let Some(disable_auto_updates) = patch.disable_auto_updates {
            config.insert(
                "disable_auto_updates".to_string(),
                serde_json::Value::Bool(disable_auto_updates),
            );
        }
        if let Some(prompt_storage) = &patch.prompt_storage {
            config.insert(
                "prompt_storage".to_string(),
                serde_json::Value::String(prompt_storage.clone()),
            );
        }

        let config_dir = self.test_home.join(".git-ai");
        fs::create_dir_all(&config_dir).expect("failed to create test HOME config directory");
        let config_path = config_dir.join("config.json");
        let serialized = serde_json::to_string(&config).expect("failed to serialize test config");
        fs::write(&config_path, serialized).expect("failed to write test HOME config");
    }

    fn apply_default_config_patch(&mut self) {
        self.patch_git_ai_config(|patch| {
            patch.exclude_prompts_in_repositories = Some(vec![]); // No exclusions = share everywhere
            patch.prompt_storage = Some("notes".to_string()); // Use notes mode for tests
        });
    }

    pub fn new() -> Self {
        if WORKTREE_MODE.with(|flag| flag.get()) {
            return Self::new_worktree_variant();
        }
        Self::new_with_mode(GitTestMode::from_env())
    }

    /// Create a worktree-backed TestRepo.
    /// This creates a normal base repo and then adds an orphan linked worktree
    /// so tests keep empty-repo semantics (the first real commit is still a root commit).
    fn new_worktree_variant() -> Self {
        let base = Self::new_with_mode(GitTestMode::from_env());

        let default_branch = default_branchname();
        let base_branch = base.current_branch();
        if base_branch == default_branch {
            let mut rng = rand::thread_rng();
            let n: u64 = rng.gen_range(0..10_000_000_000);
            let temp_branch = format!("base-worktree-{}", n);
            let temp_ref = format!("refs/heads/{}", temp_branch);
            let switch_output = Command::new(real_git_executable())
                .args([
                    "-C",
                    base.path.to_str().unwrap(),
                    "symbolic-ref",
                    "HEAD",
                    &temp_ref,
                ])
                .output()
                .expect("failed to move base repo off default branch");
            if !switch_output.status.success() {
                panic!(
                    "failed to move base repo off default branch:\nstdout: {}\nstderr: {}",
                    String::from_utf8_lossy(&switch_output.stdout),
                    String::from_utf8_lossy(&switch_output.stderr)
                );
            }
        }

        let mut rng = rand::thread_rng();
        let wt_n: u64 = rng.gen_range(0..10_000_000_000);
        let worktree_path = std::env::temp_dir().join(format!("{}-wt", wt_n));

        let output = Command::new(real_git_executable())
            .args([
                "-C",
                base.path.to_str().unwrap(),
                "worktree",
                "add",
                "--orphan",
                worktree_path.to_str().unwrap(),
            ])
            .output()
            .expect("failed to add worktree");

        if !output.status.success() {
            panic!(
                "failed to create linked worktree:\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let branch_name_output = Command::new(real_git_executable())
            .args([
                "-C",
                worktree_path.to_str().unwrap(),
                "branch",
                "--show-current",
            ])
            .output()
            .expect("failed to inspect worktree branch");
        if !branch_name_output.status.success() {
            panic!(
                "failed to inspect linked worktree branch:\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&branch_name_output.stdout),
                String::from_utf8_lossy(&branch_name_output.stderr)
            );
        }
        let current_branch = String::from_utf8_lossy(&branch_name_output.stdout)
            .trim()
            .to_string();
        if current_branch != default_branch {
            let rename_output = Command::new(real_git_executable())
                .args([
                    "-C",
                    worktree_path.to_str().unwrap(),
                    "branch",
                    "-m",
                    &default_branch,
                ])
                .output()
                .expect("failed to rename worktree branch");
            if !rename_output.status.success() {
                panic!(
                    "failed to rename linked worktree branch:\nstdout: {}\nstderr: {}",
                    String::from_utf8_lossy(&rename_output.stdout),
                    String::from_utf8_lossy(&rename_output.stderr)
                );
            }
        }

        let base_path = base.path.clone();
        let base_test_home = base.test_home.clone();
        let base_test_db_path = base.test_db_path.clone();
        let feature_flags = base.feature_flags.clone();
        let config_patch = base.config_patch.clone();
        let git_mode = base.git_mode;

        // Prevent base Drop from running - we manage cleanup in the worktree Drop
        std::mem::forget(base);

        let wt_db_n: u64 = rng.gen_range(0..10_000_000_000);
        let wt_test_db_path = std::env::temp_dir().join(format!("{}-db", wt_db_n));

        let mut repo = Self {
            path: worktree_path,
            feature_flags,
            config_patch,
            test_db_path: wt_test_db_path,
            test_home: base_test_home,
            git_mode,
            _base_repo_path: Some(base_path),
            _base_test_db_path: Some(base_test_db_path),
        };

        repo.apply_default_config_patch();
        repo.setup_git_hooks_mode();
        repo
    }

    pub fn new_with_mode(git_mode: GitTestMode) -> Self {
        let mut rng = rand::thread_rng();
        let n: u64 = rng.gen_range(0..10000000000);
        let base = std::env::temp_dir();
        let path = base.join(n.to_string());
        let test_home = base.join(format!("{}-home", n));
        let test_db_path = resolve_test_db_path(&base, n, &test_home, git_mode);
        let repo = Repository::init(&path).expect("failed to initialize git2 repository");
        let mut config = Repository::config(&repo).expect("failed to initialize git2 repository");
        config
            .set_str("user.name", "Test User")
            .expect("failed to initialize git2 repository");
        config
            .set_str("user.email", "test@example.com")
            .expect("failed to initialize git2 repository");

        let mut repo = Self {
            path,
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path,
            test_home,
            git_mode,
            _base_repo_path: None,
            _base_test_db_path: None,
        };

        // Ensure the default branch is named "main" for consistency across Git versions
        // This is important because Git 2.28+ defaults to "main" while older versions use "master"
        let _ = repo.git(&["symbolic-ref", "HEAD", "refs/heads/main"]);

        repo.apply_default_config_patch();
        repo.setup_git_hooks_mode();

        repo
    }

    pub fn new_worktree() -> Self {
        Self::new_worktree_with_mode(GitTestMode::from_env())
    }

    pub fn new_worktree_with_mode(git_mode: GitTestMode) -> Self {
        let mut rng = rand::thread_rng();
        let n: u64 = rng.gen_range(0..10000000000);
        let base = std::env::temp_dir();
        let main_path = base.join(format!("{}-main", n));
        let worktree_path = base.join(format!("{}-wt", n));
        let test_home = base.join(format!("{}-home", n));
        let test_db_path = resolve_test_db_path(&base, n, &test_home, git_mode);

        let main_repo = Repository::init(&main_path).expect("failed to initialize main repository");
        let mut main_config =
            Repository::config(&main_repo).expect("failed to initialize main repository config");
        main_config
            .set_str("user.name", "Test User")
            .expect("failed to set main user.name");
        main_config
            .set_str("user.email", "test@example.com")
            .expect("failed to set main user.email");

        let _ = Command::new(real_git_executable())
            .args([
                "-C",
                main_path.to_str().unwrap(),
                "symbolic-ref",
                "HEAD",
                "refs/heads/main",
            ])
            .output();

        let initial_commit_output = Command::new(real_git_executable())
            .args([
                "-C",
                main_path.to_str().unwrap(),
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ])
            .output()
            .expect("failed to create initial commit for worktree base");
        if !initial_commit_output.status.success() {
            panic!(
                "failed to create initial worktree base commit:\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&initial_commit_output.stdout),
                String::from_utf8_lossy(&initial_commit_output.stderr)
            );
        }

        let worktree_output = Command::new(real_git_executable())
            .args([
                "-C",
                main_path.to_str().unwrap(),
                "worktree",
                "add",
                worktree_path.to_str().unwrap(),
            ])
            .output()
            .expect("failed to create linked worktree");

        if !worktree_output.status.success() {
            panic!(
                "failed to create linked worktree:\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&worktree_output.stdout),
                String::from_utf8_lossy(&worktree_output.stderr)
            );
        }

        let mut repo = Self {
            path: worktree_path,
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path,
            test_home,
            git_mode,
            _base_repo_path: Some(main_path),
            _base_test_db_path: None,
        };

        repo.apply_default_config_patch();
        repo.setup_git_hooks_mode();
        repo
    }

    /// Create a standalone bare repository for testing
    pub fn new_bare() -> Self {
        Self::new_bare_with_mode(GitTestMode::from_env())
    }

    /// Create a standalone bare repository for testing
    pub fn new_bare_with_mode(git_mode: GitTestMode) -> Self {
        let mut rng = rand::thread_rng();
        let n: u64 = rng.gen_range(0..10000000000);
        let base = std::env::temp_dir();
        let path = base.join(n.to_string());
        let test_home = base.join(format!("{}-home", n));
        let test_db_path = resolve_test_db_path(&base, n, &test_home, git_mode);

        Repository::init_bare(&path).expect("failed to init bare repository");

        let repo = Self {
            path,
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path,
            test_home,
            git_mode,
            _base_repo_path: None,
            _base_test_db_path: None,
        };

        repo.setup_git_hooks_mode();
        repo
    }

    /// Create a pair of test repos: a local mirror and its upstream remote.
    /// The mirror is cloned from the upstream, so "origin" is automatically configured.
    /// Returns (mirror, upstream) tuple.
    ///
    /// # Example
    /// ```ignore
    /// let (mirror, upstream) = TestRepo::new_with_remote();
    ///
    /// // Make changes in mirror
    /// mirror.filename("test.txt").write("hello").stage();
    /// mirror.commit("initial commit");
    ///
    /// // Push to upstream
    /// mirror.git(&["push", "origin", "main"]);
    /// ```
    pub fn new_with_remote() -> (Self, Self) {
        Self::new_with_remote_with_mode(GitTestMode::from_env())
    }

    pub fn new_with_remote_with_mode(git_mode: GitTestMode) -> (Self, Self) {
        let mut rng = rand::thread_rng();
        let base = std::env::temp_dir();

        // Create bare upstream repository (acts as the remote server)
        let upstream_n: u64 = rng.gen_range(0..10000000000);
        let upstream_path = base.join(upstream_n.to_string());
        let upstream_test_home = base.join(format!("{}-home", upstream_n));
        let upstream_test_db_path =
            resolve_test_db_path(&base, upstream_n, &upstream_test_home, git_mode);
        Repository::init_bare(&upstream_path).expect("failed to init bare upstream repository");

        let mut upstream = Self {
            path: upstream_path.clone(),
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path: upstream_test_db_path,
            test_home: upstream_test_home,
            git_mode,
            _base_repo_path: None,
            _base_test_db_path: None,
        };

        // Ensure the upstream default branch is named "main" for consistency across Git versions
        let _ = upstream.git(&["symbolic-ref", "HEAD", "refs/heads/main"]);

        // Clone upstream to create mirror with origin configured
        let mirror_n: u64 = rng.gen_range(0..10000000000);
        let mirror_path = base.join(mirror_n.to_string());
        let mirror_test_home = base.join(format!("{}-home", mirror_n));
        let mirror_test_db_path =
            resolve_test_db_path(&base, mirror_n, &mirror_test_home, git_mode);

        let clone_output = Command::new(real_git_executable())
            .args([
                "clone",
                upstream_path.to_str().unwrap(),
                mirror_path.to_str().unwrap(),
            ])
            .output()
            .expect("failed to clone upstream repository");

        if !clone_output.status.success() {
            panic!(
                "Failed to clone upstream repository:\nstderr: {}",
                String::from_utf8_lossy(&clone_output.stderr)
            );
        }

        // Configure mirror with user credentials
        let mirror_repo =
            Repository::open(&mirror_path).expect("failed to open cloned mirror repository");
        let mut config =
            Repository::config(&mirror_repo).expect("failed to get mirror repository config");
        config
            .set_str("user.name", "Test User")
            .expect("failed to set user.name in mirror");
        config
            .set_str("user.email", "test@example.com")
            .expect("failed to set user.email in mirror");

        let mut mirror = Self {
            path: mirror_path,
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path: mirror_test_db_path,
            test_home: mirror_test_home,
            git_mode,
            _base_repo_path: None,
            _base_test_db_path: None,
        };

        // Ensure the default branch is named "main" for consistency across Git versions
        let _ = mirror.git(&["symbolic-ref", "HEAD", "refs/heads/main"]);

        upstream.apply_default_config_patch();
        mirror.apply_default_config_patch();
        upstream.setup_git_hooks_mode();
        mirror.setup_git_hooks_mode();

        (mirror, upstream)
    }

    pub fn new_at_path(path: &PathBuf) -> Self {
        Self::new_at_path_with_mode(path, GitTestMode::from_env())
    }

    pub fn new_at_path_with_mode(path: &PathBuf, git_mode: GitTestMode) -> Self {
        let mut rng = rand::thread_rng();
        let db_n: u64 = rng.gen_range(0..10000000000);
        let test_home = std::env::temp_dir().join(format!("{}-home", db_n));
        let test_db_path = resolve_test_db_path(&std::env::temp_dir(), db_n, &test_home, git_mode);
        let repo = Repository::init(path).expect("failed to initialize git2 repository");
        let mut config = Repository::config(&repo).expect("failed to initialize git2 repository");
        config
            .set_str("user.name", "Test User")
            .expect("failed to initialize git2 repository");
        config
            .set_str("user.email", "test@example.com")
            .expect("failed to initialize git2 repository");
        let mut repo = Self {
            path: path.clone(),
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path,
            test_home,
            git_mode,
            _base_repo_path: None,
            _base_test_db_path: None,
        };

        // Ensure the default branch is named "main" for consistency across Git versions
        let _ = repo.git(&["symbolic-ref", "HEAD", "refs/heads/main"]);

        repo.apply_default_config_patch();
        repo.setup_git_hooks_mode();
        repo
    }

    pub fn set_feature_flags(&mut self, feature_flags: FeatureFlags) {
        self.feature_flags = feature_flags;
    }

    fn setup_git_hooks_mode(&self) {
        if !self.git_mode.uses_hooks() {
            return;
        }

        self.sync_test_home_config_for_hooks();

        let binary_path = get_binary_path();
        let mut command = Command::new(binary_path);
        command
            .current_dir(&self.path)
            .args(["git-hooks", "ensure"]);
        self.configure_git_ai_env(&mut command);
        command.env("GIT_AI_TEST_DB_PATH", self.test_db_path.to_str().unwrap());
        command.env("GITAI_TEST_DB_PATH", self.test_db_path.to_str().unwrap());

        let output = command
            .output()
            .expect("failed to run git-ai git-hooks ensure in test setup");
        if !output.status.success() {
            panic!(
                "git-ai git-hooks ensure failed during test setup:\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
    }

    fn configure_command_env(&self, command: &mut Command) {
        if self.git_mode.uses_hooks() {
            command.env("HOME", &self.test_home);
            command.env("GIT_CONFIG_GLOBAL", self.test_home.join(".gitconfig"));
            command.env("GIT_AI_GLOBAL_GIT_HOOKS", "true");
        }

        if self.git_mode.uses_wrapper() {
            command.env("GIT_AI", "git");
        }
    }

    fn configure_git_ai_env(&self, command: &mut Command) {
        if self.git_mode.uses_hooks() {
            command.env("HOME", &self.test_home);
            command.env("GIT_CONFIG_GLOBAL", self.test_home.join(".gitconfig"));
            command.env("GIT_AI_GLOBAL_GIT_HOOKS", "true");
        }
    }

    /// Patch the git-ai config for this test repo
    /// Allows overriding specific config properties like ignore_prompts, telemetry settings, etc.
    /// The patch is applied via environment variable when running git-ai commands
    ///
    /// # Example
    /// ```ignore
    /// let mut repo = TestRepo::new();
    /// repo.patch_git_ai_config(|patch| {
    ///     patch.ignore_prompts = Some(true);
    ///     patch.telemetry_oss_disabled = Some(true);
    /// });
    /// ```
    pub fn patch_git_ai_config<F>(&mut self, f: F)
    where
        F: FnOnce(&mut ConfigPatch),
    {
        let mut patch = self.config_patch.take().unwrap_or_default();
        f(&mut patch);
        self.config_patch = Some(patch);
        self.sync_test_home_config_for_hooks();
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn canonical_path(&self) -> PathBuf {
        self.path
            .canonicalize()
            .expect("failed to canonicalize test repo path")
    }

    pub fn test_db_path(&self) -> &PathBuf {
        &self.test_db_path
    }

    pub fn test_home_path(&self) -> &PathBuf {
        &self.test_home
    }

    pub fn stats(&self) -> Result<CommitStats, String> {
        let output = self.git_ai(&["stats", "--json"])?;
        let start = output
            .find('{')
            .ok_or_else(|| format!("stats output does not contain JSON: {}", output))?;

        let mut depth = 0usize;
        let mut end_index = None;
        for (offset, ch) in output[start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    if depth == 0 {
                        return Err(format!("malformed stats JSON output: {}", output));
                    }
                    depth -= 1;
                    if depth == 0 {
                        end_index = Some(start + offset);
                        break;
                    }
                }
                _ => {}
            }
        }

        let end_index =
            end_index.ok_or_else(|| format!("incomplete stats JSON output: {}", output))?;
        let json = &output[start..=end_index];
        let stats: CommitStats =
            serde_json::from_str(json).map_err(|e| format!("invalid stats JSON: {}", e))?;
        Ok(stats)
    }

    pub fn current_branch(&self) -> String {
        self.git(&["branch", "--show-current"])
            .unwrap()
            .trim()
            .to_string()
    }

    pub fn git_ai(&self, args: &[&str]) -> Result<String, String> {
        self.git_ai_with_env(args, &[])
    }

    pub fn git(&self, args: &[&str]) -> Result<String, String> {
        self.git_with_env(args, &[], None)
    }

    /// Run a git command from a working directory (without using -C flag)
    /// This tests that git-ai correctly finds the repository root when run from a subdirectory
    /// The working_dir will be canonicalized to ensure it's an absolute path
    pub fn git_from_working_dir(
        &self,
        working_dir: &std::path::Path,
        args: &[&str],
    ) -> Result<String, String> {
        self.git_with_env(args, &[], Some(working_dir))
    }

    pub fn git_og(&self, args: &[&str]) -> Result<String, String> {
        #[cfg(windows)]
        let null_hooks = "NUL";
        #[cfg(not(windows))]
        let null_hooks = "/dev/null";

        let mut full_args: Vec<String> =
            vec!["-C".to_string(), self.path.to_str().unwrap().to_string()];
        full_args.push("-c".to_string());
        full_args.push(format!("core.hooksPath={}", null_hooks));
        full_args.extend(args.iter().map(|s| s.to_string()));

        GitAiRepository::exec_git(&full_args)
            .map(|output| {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                if stdout.is_empty() {
                    stderr
                } else if stderr.is_empty() {
                    stdout
                } else {
                    format!("{}{}", stdout, stderr)
                }
            })
            .map_err(|e| e.to_string())
    }

    pub fn benchmark_git(&self, args: &[&str]) -> Result<BenchmarkResult, String> {
        let output = self.git_with_env(args, &[("GIT_AI_DEBUG_PERFORMANCE", "2")], None)?;

        println!("output: {}", output);
        Self::parse_benchmark_result(&output)
    }

    pub fn benchmark_git_ai(&self, args: &[&str]) -> Result<BenchmarkResult, String> {
        let output = self.git_ai_with_env(args, &[("GIT_AI_DEBUG_PERFORMANCE", "2")])?;

        println!("output: {}", output);
        Self::parse_benchmark_result(&output)
    }

    fn parse_benchmark_result(output: &str) -> Result<BenchmarkResult, String> {
        // Find the JSON performance line
        for line in output.lines() {
            if line.contains("[git-ai (perf-json)]") {
                // Extract the JSON part after the colored prefix
                if let Some(json_start) = line.find('{') {
                    let json_str = &line[json_start..];
                    let parsed: serde_json::Value = serde_json::from_str(json_str)
                        .map_err(|e| format!("Failed to parse performance JSON: {}", e))?;

                    return Ok(BenchmarkResult {
                        total_duration: Duration::from_millis(
                            parsed["total_duration_ms"].as_u64().unwrap_or(0),
                        ),
                        git_duration: Duration::from_millis(
                            parsed["git_duration_ms"].as_u64().unwrap_or(0),
                        ),
                        pre_command_duration: Duration::from_millis(
                            parsed["pre_command_duration_ms"].as_u64().unwrap_or(0),
                        ),
                        post_command_duration: Duration::from_millis(
                            parsed["post_command_duration_ms"].as_u64().unwrap_or(0),
                        ),
                    });
                }
            }
        }

        Err("No performance data found in output".to_string())
    }

    pub fn git_with_env(
        &self,
        args: &[&str],
        envs: &[(&str, &str)],
        working_dir: Option<&std::path::Path>,
    ) -> Result<String, String> {
        let mut command = if self.git_mode.uses_wrapper() {
            Command::new(get_binary_path())
        } else {
            Command::new(real_git_executable())
        };

        // If working_dir is provided, use current_dir instead of -C flag
        // This tests that git-ai correctly finds the repository root when run from a subdirectory
        // The working_dir will be canonicalized to ensure it's an absolute path
        if let Some(working_dir_path) = working_dir {
            // Canonicalize to ensure we have an absolute path
            let absolute_working_dir = working_dir_path.canonicalize().map_err(|e| {
                format!(
                    "Failed to canonicalize working directory {}: {}",
                    working_dir_path.display(),
                    e
                )
            })?;
            command.args(args).current_dir(&absolute_working_dir);
        } else {
            let mut full_args = vec!["-C", self.path.to_str().unwrap()];
            full_args.extend(args);
            command.args(&full_args);
        }

        self.configure_command_env(&mut command);

        // Add config patch as environment variable if present
        if let Some(patch) = &self.config_patch
            && let Ok(patch_json) = serde_json::to_string(patch)
        {
            command.env("GIT_AI_TEST_CONFIG_PATCH", patch_json);
        }
        command.env("GIT_AI_TEST_DB_PATH", self.test_db_path.to_str().unwrap());
        command.env("GITAI_TEST_DB_PATH", self.test_db_path.to_str().unwrap());

        // Add custom environment variables
        for (key, value) in envs {
            command.env(key, value);
        }

        let output = command
            .output()
            .unwrap_or_else(|_| panic!("Failed to execute git command with env: {:?}", args));

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            // Combine stdout and stderr since git often writes to stderr
            let combined = if stdout.is_empty() {
                stderr
            } else if stderr.is_empty() {
                stdout
            } else {
                format!("{}{}", stdout, stderr)
            };
            Ok(combined)
        } else {
            Err(stderr)
        }
    }

    pub fn git_ai_from_working_dir(
        &self,
        working_dir: &std::path::Path,
        args: &[&str],
    ) -> Result<String, String> {
        let binary_path = get_binary_path();

        let mut command = Command::new(binary_path);

        let absolute_working_dir = working_dir.canonicalize().map_err(|e| {
            format!(
                "Failed to canonicalize working directory {}: {}",
                working_dir.display(),
                e
            )
        })?;
        command.args(args).current_dir(&absolute_working_dir);
        self.configure_git_ai_env(&mut command);

        if let Some(patch) = &self.config_patch
            && let Ok(patch_json) = serde_json::to_string(patch)
        {
            command.env("GIT_AI_TEST_CONFIG_PATCH", patch_json);
        }

        command.env("GIT_AI_TEST_DB_PATH", self.test_db_path.to_str().unwrap());
        command.env("GITAI_TEST_DB_PATH", self.test_db_path.to_str().unwrap());

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
            Err(stderr)
        }
    }

    pub fn git_ai_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String> {
        let binary_path = get_binary_path();

        let mut command = Command::new(binary_path);
        command.args(args).current_dir(&self.path);
        self.configure_git_ai_env(&mut command);

        // Add config patch as environment variable if present
        if let Some(patch) = &self.config_patch
            && let Ok(patch_json) = serde_json::to_string(patch)
        {
            command.env("GIT_AI_TEST_CONFIG_PATCH", patch_json);
        }

        // Add test database path for isolation
        command.env("GIT_AI_TEST_DB_PATH", self.test_db_path.to_str().unwrap());
        command.env("GITAI_TEST_DB_PATH", self.test_db_path.to_str().unwrap());

        // Add custom environment variables
        for (key, value) in envs {
            command.env(key, value);
        }

        let output = command
            .output()
            .unwrap_or_else(|_| panic!("Failed to execute git-ai command: {:?}", args));

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            // Combine stdout and stderr since git-ai often writes to stderr
            let combined = if stdout.is_empty() {
                stderr
            } else if stderr.is_empty() {
                stdout
            } else {
                format!("{}{}", stdout, stderr)
            };
            Ok(combined)
        } else {
            Err(stderr)
        }
    }

    /// Run a git-ai command with data provided on stdin
    pub fn git_ai_with_stdin(&self, args: &[&str], stdin_data: &[u8]) -> Result<String, String> {
        use std::io::Write;
        use std::process::Stdio;

        let binary_path = get_binary_path();

        let mut command = Command::new(binary_path);
        command
            .args(args)
            .current_dir(&self.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.configure_git_ai_env(&mut command);

        // Add config patch as environment variable if present
        if let Some(patch) = &self.config_patch
            && let Ok(patch_json) = serde_json::to_string(patch)
        {
            command.env("GIT_AI_TEST_CONFIG_PATCH", patch_json);
        }

        let mut child = command
            .spawn()
            .unwrap_or_else(|_| panic!("Failed to spawn git-ai command: {:?}", args));

        // Write stdin data
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(stdin_data)
                .expect("Failed to write to stdin");
        }

        let output = child
            .wait_with_output()
            .unwrap_or_else(|_| panic!("Failed to wait for git-ai command: {:?}", args));

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            // Combine stdout and stderr since git-ai often writes to stderr
            let combined = if stdout.is_empty() {
                stderr
            } else if stderr.is_empty() {
                stdout
            } else {
                format!("{}{}", stdout, stderr)
            };
            Ok(combined)
        } else {
            Err(stderr)
        }
    }

    pub fn filename(&self, filename: &str) -> TestFile<'_> {
        let file_path = self.path.join(filename);

        // If file exists, populate from existing file with blame
        if file_path.exists() {
            TestFile::from_existing_file(file_path, self)
        } else {
            // New file, start with empty lines
            TestFile::new_with_filename(file_path, vec![], self)
        }
    }

    pub fn current_working_logs(&self) -> PersistedWorkingLog {
        let repo = GitAiRepository::find_repository_in_path(self.path.to_str().unwrap())
            .expect("Failed to find repository");

        // Get the current HEAD commit SHA, or use "initial" for empty repos
        let commit_sha = repo
            .head()
            .ok()
            .and_then(|head| head.target().ok())
            .unwrap_or_else(|| "initial".to_string());

        // Get the working log for the current HEAD commit
        repo.storage.working_log_for_base_commit(&commit_sha)
    }

    pub fn commit(&self, message: &str) -> Result<NewCommit, String> {
        self.commit_with_env(message, &[], None)
    }

    /// Commit from a working directory (without using -C flag)
    /// This tests that git-ai correctly handles commits when run from a subdirectory
    /// The working_dir will be canonicalized to ensure it's an absolute path
    pub fn commit_from_working_dir(
        &self,
        working_dir: &std::path::Path,
        message: &str,
    ) -> Result<NewCommit, String> {
        self.commit_with_env(message, &[], Some(working_dir))
    }

    pub fn stage_all_and_commit(&self, message: &str) -> Result<NewCommit, String> {
        self.git(&["add", "-A"]).expect("add --all should succeed");
        self.commit(message)
    }

    pub fn commit_with_env(
        &self,
        message: &str,
        envs: &[(&str, &str)],
        working_dir: Option<&std::path::Path>,
    ) -> Result<NewCommit, String> {
        let output = self.git_with_env(&["commit", "-m", message], envs, working_dir);

        // println!("commit output: {:?}", output);
        match output {
            Ok(combined) => {
                // Get the repository and HEAD commit SHA
                let repo = GitAiRepository::find_repository_in_path(self.path.to_str().unwrap())
                    .map_err(|e| format!("Failed to find repository: {}", e))?;

                let head_commit = repo
                    .head()
                    .map_err(|e| format!("Failed to get HEAD: {}", e))?
                    .target()
                    .map_err(|e| format!("Failed to get HEAD target: {}", e))?;

                // Get the authorship log for the new commit
                let authorship_log =
                    match git_ai::git::refs::show_authorship_note(&repo, &head_commit) {
                        Some(content) => AuthorshipLog::deserialize_from_string(&content)
                            .map_err(|e| format!("Failed to parse authorship log: {}", e))?,
                        None => {
                            return Err("No authorship log found for the new commit".to_string());
                        }
                    };

                Ok(NewCommit {
                    commit_sha: head_commit,
                    authorship_log,
                    stdout: combined,
                })
            }
            Err(e) => Err(e),
        }
    }

    pub fn read_file(&self, filename: &str) -> Option<String> {
        let file_path = self.path.join(filename);
        fs::read_to_string(&file_path).ok()
    }
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        if let Some(base_path) = &self._base_repo_path {
            let _ = Command::new(real_git_executable())
                .args([
                    "-C",
                    base_path.to_str().unwrap(),
                    "worktree",
                    "remove",
                    "--force",
                    self.path.to_str().unwrap(),
                ])
                .output();

            let _ = remove_dir_all_with_retry(&self.path, 80, Duration::from_millis(50));
            let _ = remove_dir_all_with_retry(base_path, 80, Duration::from_millis(50));

            if let Some(base_db_path) = &self._base_test_db_path {
                let _ = remove_dir_all_with_retry(base_db_path, 40, Duration::from_millis(25));
            }

            let _ = remove_dir_all_with_retry(&self.test_db_path, 40, Duration::from_millis(25));
            let _ = remove_dir_all_with_retry(&self.test_home, 40, Duration::from_millis(25));
            return;
        }

        remove_dir_all_with_retry(&self.path, 80, Duration::from_millis(50))
            .expect("failed to remove test repo");
        // Also clean up the test database directory (may not exist if no DB operations were done)
        let _ = remove_dir_all_with_retry(&self.test_db_path, 40, Duration::from_millis(25));
        let _ = remove_dir_all_with_retry(&self.test_home, 40, Duration::from_millis(25));
    }
}

fn remove_dir_all_with_retry(
    path: &std::path::Path,
    attempts: usize,
    delay: Duration,
) -> std::io::Result<()> {
    for attempt in 0..attempts {
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) if should_retry_remove_dir_error(&err) => {
                if attempt + 1 == attempts {
                    return Err(err);
                }
                std::thread::sleep(delay);
            }
            Err(err) => return Err(err),
        }
    }

    Ok(())
}

fn should_retry_remove_dir_error(err: &std::io::Error) -> bool {
    if err.kind() == std::io::ErrorKind::DirectoryNotEmpty
        || err.kind() == std::io::ErrorKind::PermissionDenied
    {
        return true;
    }

    #[cfg(windows)]
    {
        // Windows can report transient file locks as `Uncategorized` with raw code 32.
        // Retry these so process teardown races don't fail otherwise-successful tests.
        if let Some(code) = err.raw_os_error() {
            return matches!(code, 5 | 32 | 145);
        }
    }

    false
}

#[derive(Debug)]
pub struct NewCommit {
    pub authorship_log: AuthorshipLog,
    pub stdout: String,
    pub commit_sha: String,
}

impl NewCommit {
    pub fn assert_authorship_snapshot(&self) {
        assert_debug_snapshot!(self.authorship_log);
    }
    pub fn print_authorship(&self) {
        // Debug method to print authorship log
        println!("{}", self.authorship_log.serialize_to_string().unwrap());
    }
}

static COMPILED_BINARY: OnceLock<PathBuf> = OnceLock::new();
static DEFAULT_BRANCH_NAME: OnceLock<String> = OnceLock::new();

pub(crate) fn real_git_executable() -> &'static str {
    git_ai::config::Config::get().git_cmd()
}

fn get_default_branch_name() -> String {
    // Since TestRepo::new() explicitly sets the default branch to "main" via symbolic-ref,
    // we always return "main" to match that behavior and ensure test consistency across
    // different Git versions and configurations.
    "main".to_string()
}

pub fn default_branchname() -> &'static str {
    DEFAULT_BRANCH_NAME.get_or_init(get_default_branch_name)
}

fn compile_binary() -> PathBuf {
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

    // Respect CARGO_TARGET_DIR if set, otherwise fall back to manifest-relative target/
    let target_dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| {
        PathBuf::from(manifest_dir)
            .join("target")
            .to_string_lossy()
            .into_owned()
    });
    #[cfg(windows)]
    {
        PathBuf::from(target_dir).join("debug/git-ai.exe")
    }

    #[cfg(not(windows))]
    {
        PathBuf::from(target_dir).join("debug/git-ai")
    }
}

pub fn get_binary_path() -> &'static PathBuf {
    COMPILED_BINARY.get_or_init(compile_binary)
}
