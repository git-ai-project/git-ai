#![allow(dead_code)]

use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::authorship::stats::CommitStats;
use git_ai::commands::core_hooks::write_core_hook_scripts;
use git_ai::config::ConfigPatch;
use git_ai::feature_flags::FeatureFlags;
use git_ai::git::repo_storage::PersistedWorkingLog;
use git_ai::git::repository as GitAiRepository;
use git_ai::observability::wrapper_performance_targets::BenchmarkResult;
use git2::Repository;
use insta::assert_debug_snapshot;
use rand::Rng;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::Duration;

use super::test_file::TestFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestGitMode {
    Wrapper,
    CoreHooks,
    WrapperWithCoreHooks,
}

impl TestGitMode {
    fn from_env() -> Self {
        match std::env::var("GIT_AI_TEST_GIT_MODE")
            .unwrap_or_else(|_| "wrapper".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "corehooks" | "core-hooks" => Self::CoreHooks,
            "wrapper+corehooks" | "wrapper-corehooks" | "both" => Self::WrapperWithCoreHooks,
            _ => Self::Wrapper,
        }
    }

    fn uses_wrapper(self) -> bool {
        matches!(self, Self::Wrapper | Self::WrapperWithCoreHooks)
    }

    fn uses_core_hooks(self) -> bool {
        matches!(self, Self::CoreHooks | Self::WrapperWithCoreHooks)
    }
}

fn test_git_mode() -> TestGitMode {
    static MODE: OnceLock<TestGitMode> = OnceLock::new();
    *MODE.get_or_init(TestGitMode::from_env)
}

#[derive(Clone, Debug)]
pub struct TestRepo {
    path: PathBuf,
    pub feature_flags: FeatureFlags,
    pub(crate) config_patch: Option<ConfigPatch>,
    test_db_path: PathBuf,
    git_mode: TestGitMode,
    core_hooks_dir: Option<PathBuf>,
}

#[allow(dead_code)]
impl Default for TestRepo {
    fn default() -> Self {
        Self::new()
    }
}

impl TestRepo {
    fn maybe_core_hooks_dir(base: &Path, suffix: u64) -> Option<PathBuf> {
        if test_git_mode().uses_core_hooks() {
            Some(base.join(format!("{}-corehooks", suffix)))
        } else {
            None
        }
    }

    fn initialize_core_hooks_if_needed(&self) {
        if let Some(hooks_dir) = &self.core_hooks_dir {
            fs::create_dir_all(hooks_dir).expect("failed to create test core hooks dir");
            write_core_hook_scripts(hooks_dir, get_binary_path())
                .expect("failed to write test core hook scripts");
        }
    }

    fn apply_default_config_patch(&mut self) {
        self.patch_git_ai_config(|patch| {
            patch.exclude_prompts_in_repositories = Some(vec![]); // No exclusions = share everywhere
            patch.prompt_storage = Some("notes".to_string()); // Use notes mode for tests
        });
    }

    pub fn new() -> Self {
        let mut rng = rand::thread_rng();
        let n: u64 = rng.gen_range(0..10000000000);
        let base = std::env::temp_dir();
        let path = base.join(n.to_string());
        // Create DB path as sibling to repo (not inside) to avoid git conflicts with WAL files
        let test_db_path = base.join(format!("{}-db", n));
        let core_hooks_dir = Self::maybe_core_hooks_dir(&base, n);
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
            git_mode: test_git_mode(),
            core_hooks_dir,
        };

        repo.apply_default_config_patch();
        repo.initialize_core_hooks_if_needed();

        repo
    }

    /// Create a standalone bare repository for testing
    pub fn new_bare() -> Self {
        let mut rng = rand::thread_rng();
        let n: u64 = rng.gen_range(0..10000000000);
        let base = std::env::temp_dir();
        let path = base.join(n.to_string());
        let test_db_path = base.join(format!("{}-db", n));
        let core_hooks_dir = Self::maybe_core_hooks_dir(&base, n);

        Repository::init_bare(&path).expect("failed to init bare repository");

        let repo = Self {
            path,
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path,
            git_mode: test_git_mode(),
            core_hooks_dir,
        };
        repo.initialize_core_hooks_if_needed();
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
        let mut rng = rand::thread_rng();
        let base = std::env::temp_dir();

        // Create bare upstream repository (acts as the remote server)
        let upstream_n: u64 = rng.gen_range(0..10000000000);
        let upstream_path = base.join(upstream_n.to_string());
        // Create DB path as sibling to repo (not inside) to avoid git conflicts with WAL files
        let upstream_test_db_path = base.join(format!("{}-db", upstream_n));
        let upstream_core_hooks_dir = Self::maybe_core_hooks_dir(&base, upstream_n);
        Repository::init_bare(&upstream_path).expect("failed to init bare upstream repository");

        let mut upstream = Self {
            path: upstream_path.clone(),
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path: upstream_test_db_path,
            git_mode: test_git_mode(),
            core_hooks_dir: upstream_core_hooks_dir,
        };

        // Clone upstream to create mirror with origin configured
        let mirror_n: u64 = rng.gen_range(0..10000000000);
        let mirror_path = base.join(mirror_n.to_string());
        // Create DB path as sibling to repo (not inside) to avoid git conflicts with WAL files
        let mirror_test_db_path = base.join(format!("{}-db", mirror_n));
        let mirror_core_hooks_dir = Self::maybe_core_hooks_dir(&base, mirror_n);

        let clone_output = Command::new(git_ai::config::Config::get().git_cmd())
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
            git_mode: test_git_mode(),
            core_hooks_dir: mirror_core_hooks_dir,
        };

        upstream.apply_default_config_patch();
        mirror.apply_default_config_patch();
        upstream.initialize_core_hooks_if_needed();
        mirror.initialize_core_hooks_if_needed();

        (mirror, upstream)
    }

    pub fn new_at_path(path: &PathBuf) -> Self {
        // Create DB path as sibling to repo (not inside) to avoid git conflicts with WAL files
        let mut rng = rand::thread_rng();
        let db_n: u64 = rng.gen_range(0..10000000000);
        let test_db_path = std::env::temp_dir().join(format!("{}-db", db_n));
        let core_hooks_dir = Self::maybe_core_hooks_dir(&std::env::temp_dir(), db_n);
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
            git_mode: test_git_mode(),
            core_hooks_dir,
        };
        repo.apply_default_config_patch();
        repo.initialize_core_hooks_if_needed();
        repo
    }

    pub fn set_feature_flags(&mut self, feature_flags: FeatureFlags) {
        self.feature_flags = feature_flags;
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

    pub fn stats(&self) -> Result<CommitStats, String> {
        let mut stats = self.git_ai(&["stats", "--json"]).unwrap();
        stats = stats.split("}}}").next().unwrap().to_string() + "}}}";
        let stats: CommitStats = serde_json::from_str(&stats).unwrap();
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
        let mut full_args: Vec<String> =
            vec!["-C".to_string(), self.path.to_str().unwrap().to_string()];
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

    fn run_git_command(
        &self,
        args: &[&str],
        envs: &[(&str, &str)],
        working_dir: Option<&Path>,
        force_c_flag: bool,
    ) -> Result<String, String> {
        let mut command = self.build_git_command(args, envs, working_dir, force_c_flag)?;
        let output = command
            .output()
            .unwrap_or_else(|_| panic!("Failed to execute git command: {:?}", args));
        Self::command_output_to_result(output)
    }

    fn build_git_command(
        &self,
        args: &[&str],
        envs: &[(&str, &str)],
        working_dir: Option<&Path>,
        force_c_flag: bool,
    ) -> Result<Command, String> {
        let mut command = if self.git_mode.uses_wrapper() {
            Command::new(get_binary_path())
        } else {
            Command::new(git_ai::config::Config::get().git_cmd())
        };

        let mut full_args: Vec<String> = Vec::new();

        if self.git_mode.uses_core_hooks() {
            let hooks_dir = self.core_hooks_dir.as_ref().ok_or_else(|| {
                "core hooks mode is enabled but no hooks dir is configured".to_string()
            })?;
            full_args.push("-c".to_string());
            full_args.push(format!("core.hooksPath={}", hooks_dir.display()));
        }

        if force_c_flag || working_dir.is_none() {
            full_args.push("-C".to_string());
            full_args.push(self.path.to_str().unwrap().to_string());
        }

        full_args.extend(args.iter().map(|arg| arg.to_string()));
        command.args(&full_args);

        if let Some(working_dir_path) = working_dir {
            let absolute_working_dir = working_dir_path.canonicalize().map_err(|e| {
                format!(
                    "Failed to canonicalize working directory {}: {}",
                    working_dir_path.display(),
                    e
                )
            })?;
            command.current_dir(absolute_working_dir);
        }

        if self.git_mode.uses_wrapper() {
            command.env("GIT_AI", "git");
        }

        if let Some(patch) = &self.config_patch
            && let Ok(patch_json) = serde_json::to_string(patch)
        {
            command.env("GIT_AI_TEST_CONFIG_PATCH", patch_json);
        }

        command.env("GIT_AI_TEST_DB_PATH", self.test_db_path.to_str().unwrap());
        for (key, value) in envs {
            command.env(key, value);
        }

        Ok(command)
    }

    fn command_output_to_result(output: Output) -> Result<String, String> {
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
        } else if stderr.is_empty() {
            Err(stdout)
        } else {
            Err(stderr)
        }
    }

    pub(crate) fn git_with_env_using_c_flag(
        &self,
        args: &[&str],
        envs: &[(&str, &str)],
        current_dir: &Path,
    ) -> Result<String, String> {
        self.run_git_command(args, envs, Some(current_dir), true)
    }

    pub fn git_with_env(
        &self,
        args: &[&str],
        envs: &[(&str, &str)],
        working_dir: Option<&std::path::Path>,
    ) -> Result<String, String> {
        self.run_git_command(args, envs, working_dir, false)
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

        if let Some(patch) = &self.config_patch
            && let Ok(patch_json) = serde_json::to_string(patch)
        {
            command.env("GIT_AI_TEST_CONFIG_PATCH", patch_json);
        }

        command.env("GIT_AI_TEST_DB_PATH", self.test_db_path.to_str().unwrap());

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

        // Add config patch as environment variable if present
        if let Some(patch) = &self.config_patch
            && let Ok(patch_json) = serde_json::to_string(patch)
        {
            command.env("GIT_AI_TEST_CONFIG_PATCH", patch_json);
        }

        // Add test database path for isolation
        command.env("GIT_AI_TEST_DB_PATH", self.test_db_path.to_str().unwrap());

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
        fs::remove_dir_all(self.path.clone()).expect("failed to remove test repo");
        // Also clean up the test database directory (may not exist if no DB operations were done)
        let _ = fs::remove_dir_all(self.test_db_path.clone());
        if let Some(core_hooks_dir) = &self.core_hooks_dir {
            let _ = fs::remove_dir_all(core_hooks_dir);
        }
    }
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

fn get_default_branch_name() -> String {
    let output = Command::new("git")
        .args(["config", "--global", "init.defaultBranch"])
        .output()
        .expect("Failed to execute git config command");

    if output.status.success() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        // Fallback to "master" if not configured
        "master".to_string()
    }
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
    PathBuf::from(target_dir).join("debug/git-ai")
}

pub(crate) fn get_binary_path() -> &'static PathBuf {
    COMPILED_BINARY.get_or_init(compile_binary)
}
