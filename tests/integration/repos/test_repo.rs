#![allow(dead_code)]

use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::authorship::stats::CommitStats;
use git_ai::config::ConfigPatch;
use git_ai::daemon::{
    ControlRequest, DaemonConfig, local_socket_connects_with_timeout, send_control_request,
    send_control_request_with_timeout,
};
use git_ai::feature_flags::FeatureFlags;
use git_ai::git::cli_parser::{ParsedGitInvocation, extract_clone_target_directory};
use git_ai::git::repo_storage::PersistedWorkingLog;
use git_ai::git::repository as GitAiRepository;
use git_ai::observability::wrapper_performance_targets::BenchmarkResult;
use insta::{Settings, assert_debug_snapshot};
use rand::Rng;
use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use super::test_file::TestFile;

const DAEMON_TEST_PROBE_TIMEOUT: Duration = Duration::from_millis(100);
const DAEMON_TEST_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(windows)]
const DAEMON_TEST_SYNC_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(not(windows))]
const DAEMON_TEST_SYNC_TOTAL_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(windows)]
const DAEMON_TEST_SYNC_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(not(windows))]
const DAEMON_TEST_SYNC_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const DAEMON_TEST_TRACE_READY_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitTestMode {
    Wrapper,
    Hooks,
    Both,
    Daemon,
    WrapperDaemon,
}

impl GitTestMode {
    pub fn from_env() -> Self {
        let mode = std::env::var("GIT_AI_TEST_GIT_MODE")
            .unwrap_or_else(|_| "wrapper".to_string())
            .to_lowercase();
        Self::from_mode_name(&mode)
    }

    pub fn from_mode_name(mode: &str) -> Self {
        match mode.to_lowercase().as_str() {
            "hooks" => Self::Hooks,
            "both" | "wrapper+hooks" | "hooks+wrapper" => Self::Both,
            "daemon" | "trace-daemon" | "pure-daemon" => Self::Daemon,
            "wrapper-daemon" => Self::WrapperDaemon,
            _ => Self::Wrapper,
        }
    }

    pub fn uses_wrapper(self) -> bool {
        matches!(self, Self::Wrapper | Self::Both | Self::WrapperDaemon)
    }

    pub fn uses_hooks(self) -> bool {
        matches!(self, Self::Hooks | Self::Both)
    }

    pub fn uses_daemon(self) -> bool {
        matches!(self, Self::Daemon | Self::WrapperDaemon)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DaemonTestScope {
    Shared,
    Dedicated,
}

#[derive(Debug, Clone)]
struct DaemonProcess {
    pid: u32,
    daemon_home: PathBuf,
    test_db_path: PathBuf,
    control_socket_path: PathBuf,
    trace_socket_path: PathBuf,
    stderr_log_path: PathBuf,
}

impl DaemonProcess {
    fn control_socket_path_for_home(test_home: &Path) -> PathBuf {
        DaemonConfig::from_home(test_home).control_socket_path
    }

    fn trace_socket_path_for_home(test_home: &Path) -> PathBuf {
        DaemonConfig::from_home(test_home).trace_socket_path
    }

    fn start(repo_path: &Path, test_home: &Path, test_db_path: &Path) -> Self {
        let control_socket_path = Self::control_socket_path_for_home(test_home);
        let trace_socket_path = Self::trace_socket_path_for_home(test_home);
        let stderr_log_path = test_home
            .join(".git-ai")
            .join("internal")
            .join("daemon")
            .join("daemon.test.stderr.log");
        fs::create_dir_all(
            stderr_log_path
                .parent()
                .expect("daemon stderr path should have parent"),
        )
        .expect("failed to create daemon log dir");
        let stderr_log = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&stderr_log_path)
            .expect("failed to create daemon stderr log");

        let mut command = Command::new(get_binary_path());
        command
            .arg("bg")
            .arg("run")
            .current_dir(test_home)
            .env("GIT_AI_TEST_DB_PATH", test_db_path)
            .env("GITAI_TEST_DB_PATH", test_db_path)
            .env("GIT_AI_DAEMON_HOME", test_home)
            .env("GIT_AI_DAEMON_CONTROL_SOCKET", &control_socket_path)
            .env("GIT_AI_DAEMON_TRACE_SOCKET", &trace_socket_path)
            .stdout(Stdio::null())
            .stderr(
                stderr_log
                    .try_clone()
                    .expect("failed to clone daemon stderr log file"),
            );
        configure_test_home_env(&mut command, test_home);

        let mut child = command
            .spawn()
            .expect("failed to spawn git-ai subprocess for test mode");
        let pid = child.id();

        let daemon = Self {
            pid,
            daemon_home: test_home.to_path_buf(),
            test_db_path: test_db_path.to_path_buf(),
            control_socket_path,
            trace_socket_path,
            stderr_log_path,
        };
        if let Err(error) = daemon.wait_until_ready(repo_path, &mut child) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{}", error);
        }
        drop(child);
        daemon
    }

    fn wait_until_ready(&self, repo_path: &Path, child: &mut Child) -> Result<(), String> {
        let repo_working_dir = repo_path.to_string_lossy().to_string();
        let mut last_status_error: Option<String> = None;
        for _ in 0..1200 {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| format!("failed polling daemon child status: {}", e))?
            {
                let stderr_tail = self.read_stderr_tail();
                return Err(format!(
                    "daemon exited before becoming ready (pid {}, status {}): sockets {} {}{}",
                    self.pid,
                    status,
                    self.control_socket_path.display(),
                    self.trace_socket_path.display(),
                    stderr_tail
                ));
            }

            #[cfg(unix)]
            {
                if !is_process_alive(self.pid) {
                    let stderr_tail = self.read_stderr_tail();
                    return Err(format!(
                        "daemon exited before becoming ready (pid {}): sockets {} {}",
                        self.pid,
                        self.control_socket_path.display(),
                        self.trace_socket_path.display()
                    ) + &stderr_tail);
                }
            }

            let status = send_control_request_with_timeout(
                &self.control_socket_path,
                &ControlRequest::StatusFamily {
                    repo_working_dir: repo_working_dir.clone(),
                },
                DAEMON_TEST_CONTROL_TIMEOUT,
            );
            match status {
                Ok(response) => {
                    if local_socket_connects_with_timeout(
                        &self.trace_socket_path,
                        DAEMON_TEST_PROBE_TIMEOUT,
                    )
                    .is_ok()
                    {
                        let baseline_seq = response
                            .data
                            .as_ref()
                            .and_then(|data| data.get("latest_seq"))
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        self.wait_until_trace_pipeline_ready(
                            repo_path,
                            &repo_working_dir,
                            baseline_seq,
                        )?;
                        return Ok(());
                    }
                }
                Err(error) => {
                    last_status_error = Some(error.to_string());
                }
            }
            thread::sleep(Duration::from_millis(25));
        }

        let stderr_tail = self.read_stderr_tail();
        Err(format!(
            "daemon did not become ready at {} (trace socket: {}, last_status_error={})",
            self.control_socket_path.display(),
            self.trace_socket_path.display(),
            last_status_error.as_deref().unwrap_or("none")
        ) + &stderr_tail)
    }

    fn wait_until_trace_pipeline_ready(
        &self,
        repo_path: &Path,
        repo_working_dir: &str,
        baseline_seq: u64,
    ) -> Result<(), String> {
        #[cfg(windows)]
        let null_hooks = "NUL";
        #[cfg(not(windows))]
        let null_hooks = "/dev/null";

        let mut command = Command::new(real_git_executable());
        command
            .arg("-C")
            .arg(repo_path)
            .arg("-c")
            .arg(format!("core.hooksPath={}", null_hooks))
            .args(["notes", "--ref=ai", "list"])
            .env(
                "GIT_TRACE2_EVENT",
                DaemonConfig::trace2_event_target_for_path(&self.trace_socket_path),
            )
            .env("GIT_TRACE2_EVENT_NESTING", "10");
        configure_test_home_env(&mut command, &self.daemon_home);

        let output = command.output().map_err(|error| {
            format!(
                "failed to run daemon readiness probe git notes list: {}",
                error
            )
        })?;
        if !output.status.success() {
            return Err(format!(
                "daemon readiness probe git notes list failed:\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let start = Instant::now();
        while start.elapsed() < DAEMON_TEST_TRACE_READY_TIMEOUT {
            let response = send_control_request_with_timeout(
                &self.control_socket_path,
                &ControlRequest::StatusFamily {
                    repo_working_dir: repo_working_dir.to_string(),
                },
                DAEMON_TEST_CONTROL_TIMEOUT,
            )
            .map_err(|error| format!("failed polling daemon readiness seq: {}", error))?;
            let latest_seq = response
                .data
                .as_ref()
                .and_then(|data| data.get("latest_seq"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if latest_seq > baseline_seq {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(25));
        }

        Err(format!(
            "daemon trace pipeline did not advance latest_seq beyond {} for {}",
            baseline_seq, repo_working_dir
        ))
    }

    fn read_stderr_tail(&self) -> String {
        let mut file = match fs::File::open(&self.stderr_log_path) {
            Ok(file) => file,
            Err(_) => return String::new(),
        };
        let mut content = String::new();
        if file.read_to_string(&mut content).is_err() {
            return String::new();
        }
        if content.trim().is_empty() {
            return String::new();
        }
        let mut lines: Vec<&str> = content.lines().collect();
        if lines.len() > 20 {
            lines = lines.split_off(lines.len() - 20);
        }
        format!(
            "\nDaemon stderr tail ({})\n{}",
            self.stderr_log_path.display(),
            lines.join("\n")
        )
    }

    fn shutdown(&self) {
        let _ = send_control_request(&self.control_socket_path, &ControlRequest::Shutdown);

        #[cfg(unix)]
        {
            for _ in 0..200 {
                if reap_child_if_exited(self.pid) {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }

            let _ = unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
            for _ in 0..100 {
                if reap_child_if_exited(self.pid) {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }

        #[cfg(not(unix))]
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &self.pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .output();
        }
    }
}

fn configure_test_home_env(command: &mut Command, test_home: &Path) {
    command.env("HOME", test_home);
    command.env("GIT_CONFIG_GLOBAL", test_home.join(".gitconfig"));
    #[cfg(windows)]
    {
        command.env("USERPROFILE", test_home);
        command.env("APPDATA", test_home.join("AppData").join("Roaming"));
        command.env("LOCALAPPDATA", test_home.join("AppData").join("Local"));
    }
}

static SHARED_DAEMON_PROCESS: OnceLock<Arc<DaemonProcess>> = OnceLock::new();
static SHARED_DAEMON_POOL: OnceLock<Mutex<HashMap<usize, Arc<DaemonProcess>>>> = OnceLock::new();
static SHARED_DAEMON_EXIT_HOOK: OnceLock<()> = OnceLock::new();
static DAEMON_SYNC_REGISTRY: OnceLock<Mutex<DaemonSyncRegistry>> = OnceLock::new();
static SHARED_DAEMON_POOL_ASSIGNMENT_COUNTER: AtomicUsize = AtomicUsize::new(0);
static TEST_SYNC_SESSION_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn new_daemon_test_sync_session_id() -> String {
    let id = TEST_SYNC_SESSION_COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
    format!("test-sync-{}-{}", std::process::id(), id)
}

fn shared_daemon_pool_size() -> usize {
    std::env::var("GIT_AI_TEST_SHARED_DAEMON_POOL_SIZE")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|size| *size > 0)
        .unwrap_or(8)
}

extern "C" fn shutdown_shared_daemon_at_process_exit() {
    if let Some(daemon) = SHARED_DAEMON_PROCESS.get() {
        daemon.shutdown();
    }
    if let Some(pool) = SHARED_DAEMON_POOL.get() {
        let daemons = {
            let mut pool = pool.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            pool.drain().map(|(_, daemon)| daemon).collect::<Vec<_>>()
        };
        for daemon in daemons {
            daemon.shutdown();
        }
    }
}

fn register_shared_daemon_exit_hook() {
    SHARED_DAEMON_EXIT_HOOK.get_or_init(|| {
        let rc = unsafe { libc::atexit(shutdown_shared_daemon_at_process_exit) };
        assert_eq!(rc, 0, "failed to register shared daemon exit hook");
    });
}

fn shared_daemon_process(repo_path: &Path) -> Arc<DaemonProcess> {
    register_shared_daemon_exit_hook();
    let pool_size = shared_daemon_pool_size();
    if pool_size <= 1 {
        return SHARED_DAEMON_PROCESS
            .get_or_init(|| Arc::new(start_shared_daemon_process(repo_path, None)))
            .clone();
    }

    let shard = shared_daemon_pool_shard(pool_size);
    let pool = SHARED_DAEMON_POOL.get_or_init(|| Mutex::new(HashMap::new()));
    let mut pool = pool.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    pool.entry(shard)
        .or_insert_with(|| Arc::new(start_shared_daemon_process(repo_path, Some(shard))))
        .clone()
}

fn start_shared_daemon_process(repo_path: &Path, shard: Option<usize>) -> DaemonProcess {
    let mut rng = rand::thread_rng();
    let n: u64 = rng.gen_range(0..10_000_000_000);
    let base = std::env::temp_dir();
    let shard_suffix = shard
        .map(|shard| format!("-pool-{}", shard))
        .unwrap_or_default();
    let daemon_home = base.join(format!("git-ai-shared-daemon-{}{}-home", n, shard_suffix));
    let test_db_path = base.join(format!("git-ai-shared-daemon-{}{}-db", n, shard_suffix));
    DaemonProcess::start(repo_path, &daemon_home, &test_db_path)
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        self.shutdown();
    }
}

thread_local! {
    static WORKTREE_MODE: Cell<bool> = const { Cell::new(false) };
    static SHARED_DAEMON_POOL_SHARD: Cell<Option<usize>> = const { Cell::new(None) };
}

fn shared_daemon_pool_shard(pool_size: usize) -> usize {
    if pool_size <= 1 {
        return 0;
    }

    SHARED_DAEMON_POOL_SHARD.with(|slot| match slot.get() {
        Some(shard) if shard < pool_size => shard,
        _ => {
            let shard =
                SHARED_DAEMON_POOL_ASSIGNMENT_COUNTER.fetch_add(1, Ordering::Relaxed) % pool_size;
            slot.set(Some(shard));
            shard
        }
    })
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

#[derive(Debug, Default)]
struct DaemonSyncRegistry {
    last_synced_completion_count: HashMap<String, u64>,
    pending_sessions: HashMap<String, Vec<String>>,
}

impl DaemonSyncRegistry {
    fn pending_sessions(&self, family_key: &str) -> Vec<String> {
        self.pending_sessions
            .get(family_key)
            .cloned()
            .unwrap_or_default()
    }

    fn last_synced_completion_count(&self, family_key: &str) -> u64 {
        self.last_synced_completion_count
            .get(family_key)
            .copied()
            .unwrap_or(0)
    }

    fn record_expected_completion_session(&mut self, family_key: &str, session: &str) {
        self.pending_sessions
            .entry(family_key.to_string())
            .or_default()
            .push(session.to_string());
    }

    fn clear_pending_sessions(&mut self, family_key: &str) {
        self.pending_sessions.remove(family_key);
    }

    fn advance_last_synced_completion_count(&mut self, family_key: &str, completion_count: u64) {
        let entry = self
            .last_synced_completion_count
            .entry(family_key.to_string())
            .or_insert(0);
        *entry = (*entry).max(completion_count);
    }

    fn mark_synced_through(&mut self, family_key: &str, completion_count: u64) {
        self.advance_last_synced_completion_count(family_key, completion_count);
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct DaemonTestCompletionLogEntry {
    #[serde(default)]
    pub(crate) seq: u64,
    #[serde(default)]
    pub(crate) primary_command: Option<String>,
    #[serde(default)]
    pub(crate) exit_code: Option<i32>,
    #[serde(default)]
    pub(crate) sync_tracked: bool,
    #[serde(default)]
    pub(crate) test_sync_session: Option<String>,
    pub(crate) status: String,
    pub(crate) error: Option<String>,
}

fn daemon_sync_registry() -> &'static Mutex<DaemonSyncRegistry> {
    DAEMON_SYNC_REGISTRY.get_or_init(|| Mutex::new(DaemonSyncRegistry::default()))
}

pub(crate) fn git_primary_command<'a>(args: &'a [&'a str]) -> Option<(&'a str, Option<&'a str>)> {
    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        match arg {
            "-c" | "-C" | "--config-env" | "--exec-path" | "--git-dir" | "--namespace"
            | "--super-prefix" | "--work-tree" => {
                iter.next();
            }
            _ if arg.starts_with("-c")
                || arg.starts_with("-C")
                || arg.starts_with("--config-env=")
                || arg.starts_with("--exec-path=")
                || arg.starts_with("--git-dir=")
                || arg.starts_with("--namespace=")
                || arg.starts_with("--super-prefix=")
                || arg.starts_with("--work-tree=") => {}
            _ if arg.starts_with('-') => {}
            _ => return Some((arg, iter.next().filter(|next| !next.starts_with('-')))),
        }
    }
    None
}

pub(crate) fn git_command_routes_to_clone_target(args: &[&str]) -> bool {
    git_primary_command(args).map(|(command, _)| command) == Some("clone")
}

pub(crate) fn git_command_requires_daemon_sync(args: &[&str]) -> bool {
    let Some((command, _subcommand)) = git_primary_command(args) else {
        return false;
    };

    matches!(command, "notes")
}

fn git_ai_primary_command<'a>(args: &'a [&'a str]) -> Option<&'a str> {
    args.iter().copied().find(|arg| !arg.starts_with('-'))
}

fn is_known_checkpoint_preset(arg: &str) -> bool {
    matches!(
        arg,
        "claude"
            | "codex"
            | "continue-cli"
            | "cursor"
            | "gemini"
            | "github-copilot"
            | "amp"
            | "windsurf"
            | "opencode"
            | "ai_tab"
            | "mock_ai"
            | "droid"
            | "agent-v1"
    )
}

fn normalize_test_git_ai_checkpoint_args(args: &[&str]) -> Vec<String> {
    let original = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    if git_ai_primary_command(args) != Some("checkpoint") || args.len() <= 1 {
        return original;
    }

    if args.contains(&"--") {
        return original;
    }

    let mut normalized = vec![args[0].to_string()];
    let mut i = 1usize;
    while i < args.len() {
        match args[i] {
            "--reset" => {
                normalized.push(args[i].to_string());
                i += 1;
            }
            "--hook-input" => {
                normalized.push(args[i].to_string());
                if let Some(value) = args.get(i + 1) {
                    normalized.push((*value).to_string());
                }
                i += 2;
            }
            arg if arg.starts_with("--hook-input=") || arg.starts_with('-') => {
                normalized.push(arg.to_string());
                i += 1;
            }
            arg if is_known_checkpoint_preset(arg) => return original,
            _ => {
                normalized.push("--".to_string());
                normalized.extend(args[i..].iter().map(|arg| (*arg).to_string()));
                return normalized;
            }
        }
    }

    normalized
}

fn git_ai_command_requires_daemon_sync(args: &[&str]) -> bool {
    matches!(
        git_ai_primary_command(args),
        Some("blame" | "continue" | "diff" | "prompts" | "search" | "stats")
    )
}

fn git_invocation_requires_daemon_sync(invocation: &ParsedGitInvocation) -> bool {
    matches!(invocation.command.as_deref(), Some("notes"))
}

fn git_invocation_routes_to_clone_target(invocation: &ParsedGitInvocation) -> bool {
    invocation.command.as_deref() == Some("clone")
}

fn clone_target_path(args: &[&str], cwd: &Path) -> Option<PathBuf> {
    let argv = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let clone_index = argv.iter().position(|arg| arg == "clone")?;
    let target = extract_clone_target_directory(&argv[clone_index + 1..])?;
    let target_path = PathBuf::from(target);
    let resolved = if target_path.is_absolute() {
        target_path
    } else {
        cwd.join(target_path)
    };
    Some(resolved.canonicalize().unwrap_or(resolved))
}

fn env_explicitly_enables_trace2(envs: &[(&str, &str)]) -> bool {
    envs.iter().any(|(key, value)| {
        matches!(*key, "GIT_TRACE2" | "GIT_TRACE2_EVENT" | "GIT_TRACE2_PERF")
            && !matches!(*value, "" | "0")
    })
}

#[derive(Debug)]
pub struct TestRepo {
    path: PathBuf,
    pub feature_flags: FeatureFlags,
    pub(crate) config_patch: Option<ConfigPatch>,
    test_db_path: PathBuf,
    test_home: PathBuf,
    git_mode: GitTestMode,
    daemon_scope: DaemonTestScope,
    daemon_process: Option<Arc<DaemonProcess>>,
    /// When this TestRepo is backed by a linked worktree, holds the base repo path
    /// so we can clean it up on drop.
    _base_repo_path: Option<PathBuf>,
    /// Base repo's test DB path for cleanup.
    _base_test_db_path: Option<PathBuf>,
    daemon_family_key: OnceLock<String>,
}

#[allow(dead_code)]
impl Default for TestRepo {
    fn default() -> Self {
        Self::new()
    }
}

impl TestRepo {
    fn parsed_git_invocation_for_tracking(
        &self,
        args: &[&str],
        repo_context: Option<&Path>,
    ) -> ParsedGitInvocation {
        let argv = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        let cwd = repo_context.unwrap_or_else(|| self.path().as_path());
        git_ai::daemon::test_sync::tracked_parsed_git_invocation_for_test_sync(&argv, cwd)
    }

    pub(crate) fn git_command_affects_daemon_for_tracking(
        &self,
        args: &[&str],
        repo_context: Option<&Path>,
    ) -> bool {
        let parsed = self.parsed_git_invocation_for_tracking(args, repo_context);
        git_ai::daemon::test_sync::tracks_parsed_git_invocation_for_test_sync(&parsed)
    }

    pub fn new_with_daemon_scope(daemon_scope: DaemonTestScope) -> Self {
        if WORKTREE_MODE.with(|flag| flag.get()) {
            return Self::new_worktree_variant_with_daemon_scope(daemon_scope);
        }
        Self::new_with_mode_and_daemon_scope(GitTestMode::from_env(), daemon_scope)
    }

    pub fn new_dedicated_daemon() -> Self {
        Self::new_with_daemon_scope(DaemonTestScope::Dedicated)
    }

    fn write_test_config_to_home(&self, home: &Path) {
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
        if let Some(custom_attributes) = &patch.custom_attributes {
            let attrs_map: serde_json::Map<String, serde_json::Value> = custom_attributes
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect();
            config.insert(
                "custom_attributes".to_string(),
                serde_json::Value::Object(attrs_map),
            );
        }
        if let Some(feature_flags) = &patch.feature_flags {
            config.insert("feature_flags".to_string(), feature_flags.clone());
        }

        let config_dir = home.join(".git-ai");
        fs::create_dir_all(&config_dir).expect("failed to create test HOME config directory");
        let config_path = config_dir.join("config.json");
        let serialized = serde_json::to_string(&config).expect("failed to serialize test config");
        fs::write(&config_path, serialized).expect("failed to write test HOME config");
    }

    fn sync_test_home_config_for_hooks(&self) {
        if !self.git_mode.uses_hooks() && !self.git_mode.uses_daemon() {
            return;
        }
        self.write_test_config_to_home(&self.test_home);
        if let Some(daemon) = &self.daemon_process
            && daemon.daemon_home != self.test_home
        {
            self.write_test_config_to_home(&daemon.daemon_home);
        }
    }

    fn apply_default_config_patch(&mut self) {
        let git_mode = self.git_mode;
        self.patch_git_ai_config(|patch| {
            patch.exclude_prompts_in_repositories = Some(vec![]); // No exclusions = share everywhere
            patch.prompt_storage = Some("notes".to_string()); // Use notes mode for tests
            if git_mode == GitTestMode::WrapperDaemon {
                patch.feature_flags = Some(serde_json::json!({
                    "async_mode": true
                }));
            }
        });
    }

    pub fn new() -> Self {
        Self::new_with_daemon_scope(DaemonTestScope::Shared)
    }

    /// Create a worktree-backed TestRepo.
    /// This creates a normal base repo and then adds an orphan linked worktree
    /// so tests keep empty-repo semantics (the first real commit is still a root commit).
    fn new_worktree_variant() -> Self {
        Self::new_worktree_variant_with_daemon_scope(DaemonTestScope::Shared)
    }

    fn new_worktree_variant_with_daemon_scope(daemon_scope: DaemonTestScope) -> Self {
        let mut base = Self::new_with_mode_and_daemon_scope(GitTestMode::from_env(), daemon_scope);

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
                    default_branch,
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
        let daemon_scope = base.daemon_scope;
        let daemon_process = base.daemon_process.take();

        // Prevent base Drop from running - we manage cleanup in the worktree Drop
        std::mem::forget(base);

        let wt_test_db_path = if git_mode.uses_daemon() {
            // Daemon mode uses a single process-scoped internal DB path.
            // Reuse the base DB path for linked worktrees so test expectations and daemon writes align.
            base_test_db_path.clone()
        } else {
            let wt_db_n: u64 = rng.gen_range(0..10_000_000_000);
            std::env::temp_dir().join(format!("{}-db", wt_db_n))
        };

        let mut repo = Self {
            path: worktree_path,
            feature_flags,
            config_patch,
            test_db_path: wt_test_db_path,
            test_home: base_test_home,
            git_mode,
            daemon_scope,
            daemon_process,
            _base_repo_path: Some(base_path),
            _base_test_db_path: Some(base_test_db_path),
            daemon_family_key: OnceLock::new(),
        };

        repo.apply_default_config_patch();
        repo.setup_git_hooks_mode();
        repo
    }

    pub fn new_with_mode(git_mode: GitTestMode) -> Self {
        Self::new_with_mode_and_daemon_scope(git_mode, DaemonTestScope::Shared)
    }

    pub fn new_with_mode_and_daemon_scope(
        git_mode: GitTestMode,
        daemon_scope: DaemonTestScope,
    ) -> Self {
        let mut rng = rand::thread_rng();
        let n: u64 = rng.gen_range(0..10000000000);
        let base = std::env::temp_dir();
        let path = base.join(n.to_string());
        let test_home = base.join(format!("{}-home", n));
        let test_db_path = resolve_test_db_path(&base, n, &test_home, git_mode);

        // Clone from cached template (git init + config + symbolic-ref already done)
        clone_template_to(&path);

        let mut repo = Self {
            path,
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path,
            test_home,
            git_mode,
            daemon_scope,
            daemon_process: None,
            _base_repo_path: None,
            _base_test_db_path: None,
            daemon_family_key: OnceLock::new(),
        };

        repo.apply_default_config_patch();
        repo.setup_daemon_mode();
        repo.setup_git_hooks_mode();

        repo
    }

    pub fn new_worktree() -> Self {
        Self::new_worktree_with_mode(GitTestMode::from_env())
    }

    pub fn new_worktree_with_mode(git_mode: GitTestMode) -> Self {
        Self::new_worktree_with_mode_and_daemon_scope(git_mode, DaemonTestScope::Shared)
    }

    pub fn new_worktree_with_mode_and_daemon_scope(
        git_mode: GitTestMode,
        daemon_scope: DaemonTestScope,
    ) -> Self {
        let mut rng = rand::thread_rng();
        let n: u64 = rng.gen_range(0..10000000000);
        let base = std::env::temp_dir();
        let main_path = base.join(format!("{}-main", n));
        let worktree_path = base.join(format!("{}-wt", n));
        let test_home = base.join(format!("{}-home", n));
        let test_db_path = resolve_test_db_path(&base, n, &test_home, git_mode);

        // Clone from cached template (git init + config + symbolic-ref already done)
        clone_template_to(&main_path);

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
            daemon_scope,
            daemon_process: None,
            _base_repo_path: Some(main_path),
            _base_test_db_path: None,
            daemon_family_key: OnceLock::new(),
        };

        repo.apply_default_config_patch();
        repo.setup_daemon_mode();
        repo.setup_git_hooks_mode();
        repo
    }

    /// Create a standalone bare repository for testing
    pub fn new_bare() -> Self {
        Self::new_bare_with_mode(GitTestMode::from_env())
    }

    /// Create a standalone bare repository for testing
    pub fn new_bare_with_mode(git_mode: GitTestMode) -> Self {
        Self::new_bare_with_mode_and_daemon_scope(git_mode, DaemonTestScope::Shared)
    }

    pub fn new_bare_with_mode_and_daemon_scope(
        git_mode: GitTestMode,
        daemon_scope: DaemonTestScope,
    ) -> Self {
        let mut rng = rand::thread_rng();
        let n: u64 = rng.gen_range(0..10000000000);
        let base = std::env::temp_dir();
        let path = base.join(n.to_string());
        let test_home = base.join(format!("{}-home", n));
        let test_db_path = resolve_test_db_path(&base, n, &test_home, git_mode);

        // Clone from cached bare template
        clone_bare_template_to(&path);

        let repo = Self {
            path,
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path,
            test_home,
            git_mode,
            daemon_scope,
            daemon_process: None,
            _base_repo_path: None,
            _base_test_db_path: None,
            daemon_family_key: OnceLock::new(),
        };

        let mut repo = repo;
        repo.setup_daemon_mode();
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
        Self::new_with_remote_with_mode_and_daemon_scope(git_mode, DaemonTestScope::Shared)
    }

    pub fn new_with_remote_with_mode_and_daemon_scope(
        git_mode: GitTestMode,
        daemon_scope: DaemonTestScope,
    ) -> (Self, Self) {
        let mut rng = rand::thread_rng();
        let base = std::env::temp_dir();

        // Create bare upstream repository (acts as the remote server)
        let upstream_n: u64 = rng.gen_range(0..10000000000);
        let upstream_path = base.join(upstream_n.to_string());
        let upstream_test_home = base.join(format!("{}-home", upstream_n));
        let upstream_test_db_path =
            resolve_test_db_path(&base, upstream_n, &upstream_test_home, git_mode);
        clone_bare_template_to(&upstream_path);

        let mut upstream = Self {
            path: upstream_path.clone(),
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path: upstream_test_db_path,
            test_home: upstream_test_home,
            git_mode,
            daemon_scope,
            daemon_process: None,
            _base_repo_path: None,
            _base_test_db_path: None,
            daemon_family_key: OnceLock::new(),
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
        set_repo_user_config(&mirror_path);

        let mut mirror = Self {
            path: mirror_path,
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path: mirror_test_db_path,
            test_home: mirror_test_home,
            git_mode,
            daemon_scope,
            daemon_process: None,
            _base_repo_path: None,
            _base_test_db_path: None,
            daemon_family_key: OnceLock::new(),
        };

        // Ensure the default branch is named "main" for consistency across Git versions
        let _ = mirror.git(&["symbolic-ref", "HEAD", "refs/heads/main"]);

        upstream.apply_default_config_patch();
        mirror.apply_default_config_patch();
        mirror.setup_daemon_mode();
        // The upstream side of new_with_remote() is a bare remote fixture. It is not the repo
        // under test for daemon mode, and bootstrapping the shared daemon against a bare repo
        // breaks the readiness handshake for this test process.
        upstream.setup_git_hooks_mode();
        mirror.setup_git_hooks_mode();

        (mirror, upstream)
    }

    pub fn new_at_path(path: &Path) -> Self {
        Self::new_at_path_with_mode(path, GitTestMode::from_env())
    }

    pub fn new_at_path_with_mode(path: &Path, git_mode: GitTestMode) -> Self {
        Self::new_at_path_with_mode_and_daemon_scope(path, git_mode, DaemonTestScope::Shared)
    }

    pub fn new_at_path_with_mode_and_daemon_scope(
        path: &Path,
        git_mode: GitTestMode,
        daemon_scope: DaemonTestScope,
    ) -> Self {
        let mut rng = rand::thread_rng();
        let db_n: u64 = rng.gen_range(0..10000000000);
        let test_home = std::env::temp_dir().join(format!("{}-home", db_n));
        let test_db_path = resolve_test_db_path(&std::env::temp_dir(), db_n, &test_home, git_mode);

        // Clone from cached template (git init + config + symbolic-ref already done).
        // If path already has a .git directory (e.g. a real repo cloned from GitHub),
        // skip the template copy to avoid overwriting its config, HEAD, and refs.
        if path.join(".git").exists() {
            set_repo_user_config(path);
        } else {
            clone_template_to(path);
        }

        let mut repo = Self {
            path: path.to_path_buf(),
            feature_flags: FeatureFlags::default(),
            config_patch: None,
            test_db_path,
            test_home,
            git_mode,
            daemon_scope,
            daemon_process: None,
            _base_repo_path: None,
            _base_test_db_path: None,
            daemon_family_key: OnceLock::new(),
        };

        repo.apply_default_config_patch();
        repo.setup_daemon_mode();
        repo.setup_git_hooks_mode();
        repo
    }

    pub fn set_feature_flags(&mut self, feature_flags: FeatureFlags) {
        self.feature_flags = feature_flags;
    }

    pub(crate) fn daemon_control_socket_path(&self) -> PathBuf {
        self.daemon_process
            .as_ref()
            .map(|daemon| daemon.control_socket_path.clone())
            .unwrap_or_else(|| DaemonProcess::control_socket_path_for_home(&self.test_home))
    }

    pub(crate) fn daemon_home_path(&self) -> PathBuf {
        self.daemon_process
            .as_ref()
            .map(|daemon| daemon.daemon_home.clone())
            .unwrap_or_else(|| self.test_home.clone())
    }

    pub(crate) fn daemon_trace_socket_path(&self) -> PathBuf {
        self.daemon_process
            .as_ref()
            .map(|daemon| daemon.trace_socket_path.clone())
            .unwrap_or_else(|| DaemonProcess::trace_socket_path_for_home(&self.test_home))
    }

    pub(crate) fn config_patch_json(&self) -> Option<String> {
        self.config_patch
            .as_ref()
            .and_then(|patch| serde_json::to_string(patch).ok())
    }

    fn trace2_nesting_value() -> String {
        std::env::var("GIT_AI_TEST_TRACE2_NESTING").unwrap_or_else(|_| "10".to_string())
    }

    fn setup_daemon_mode(&mut self) {
        if !self.git_mode.uses_daemon() {
            return;
        }
        if self.daemon_process.is_some() {
            return;
        }
        let daemon = match self.daemon_scope {
            DaemonTestScope::Shared => shared_daemon_process(&self.path),
            DaemonTestScope::Dedicated => Arc::new(DaemonProcess::start(
                &self.path,
                &self.test_home,
                &self.test_db_path,
            )),
        };
        self.test_db_path = daemon.test_db_path.clone();
        self.daemon_process = Some(daemon);
        self.sync_test_home_config_for_hooks();
    }

    fn daemon_completion_log_path_for_family(&self, family_key: &str) -> PathBuf {
        DaemonConfig::from_home(&self.daemon_home_path())
            .test_completion_log_path_for_family(family_key)
    }

    pub(crate) fn daemon_total_completion_count(&self) -> u64 {
        let family_key = self.daemon_family_key();
        self.daemon_completion_entries_for_family(&family_key).len() as u64
    }

    pub(crate) fn daemon_completion_entries(&self) -> Vec<DaemonTestCompletionLogEntry> {
        let family_key = self.daemon_family_key();
        self.daemon_completion_entries_for_family(&family_key)
    }

    fn daemon_completion_entries_for_family(
        &self,
        family_key: &str,
    ) -> Vec<DaemonTestCompletionLogEntry> {
        let path = self.daemon_completion_log_path_for_family(family_key);
        let Ok(content) = fs::read_to_string(&path) else {
            return Vec::new();
        };
        let ends_with_newline = content.ends_with('\n');
        let lines: Vec<&str> = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let total_lines = lines.len();

        lines
            .into_iter()
            .enumerate()
            .filter_map(|(idx, line)| {
                match serde_json::from_str::<DaemonTestCompletionLogEntry>(line) {
                    Ok(entry) => Some(entry),
                    Err(error)
                        if idx + 1 == total_lines && !ends_with_newline && error.is_eof() =>
                    {
                        None
                    }
                    Err(error) => {
                        panic!(
                            "failed to parse daemon completion log entry {} in {}: {}",
                            idx + 1,
                            path.display(),
                            error
                        )
                    }
                }
            })
            .collect()
    }

    fn wait_for_daemon_completion_count(
        &self,
        family_key: &str,
        baseline_count: u64,
        expected_count: u64,
    ) -> u64 {
        let start = Instant::now();
        let mut last_progress = start;
        let mut last_observed_count = baseline_count;
        loop {
            let entries = self.daemon_completion_entries_for_family(family_key);
            let tracked_entries = entries
                .iter()
                .filter(|entry| entry.sync_tracked)
                .collect::<Vec<_>>();
            if let Some(error_entry) = tracked_entries
                .iter()
                .skip(baseline_count as usize)
                .find(|entry| entry.status == "error")
            {
                panic!(
                    "daemon completion log reported an error for family {}: {}",
                    family_key,
                    error_entry
                        .error
                        .as_deref()
                        .unwrap_or("unknown completion error")
                );
            }
            let observed_count = tracked_entries.len() as u64;
            if observed_count >= expected_count {
                return observed_count;
            }
            if observed_count > last_observed_count {
                last_progress = Instant::now();
                last_observed_count = observed_count;
            }
            if start.elapsed() >= DAEMON_TEST_SYNC_TOTAL_TIMEOUT
                || last_progress.elapsed() >= DAEMON_TEST_SYNC_IDLE_TIMEOUT
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        panic!(
            "daemon completion log for family {} did not reach {} entries within timeout",
            family_key, expected_count
        );
    }

    fn wait_for_daemon_completion_sessions(&self, family_key: &str, sessions: &[String]) -> u64 {
        let expected: std::collections::HashSet<&str> =
            sessions.iter().map(|session| session.as_str()).collect();
        let start = Instant::now();
        let mut last_progress = start;
        let mut last_observed_count = 0usize;
        let mut last_completed_count = 0usize;
        loop {
            let entries = self.daemon_completion_entries_for_family(family_key);
            let tracked_entries = entries
                .iter()
                .filter(|entry| entry.sync_tracked)
                .collect::<Vec<_>>();
            let mut completed = std::collections::HashSet::<&str>::new();

            for entry in &tracked_entries {
                let Some(session) = entry.test_sync_session.as_deref() else {
                    continue;
                };
                if !expected.contains(session) {
                    continue;
                }
                if entry.status == "error" {
                    panic!(
                        "daemon completion log reported an error for family {} session {}: {}",
                        family_key,
                        session,
                        entry.error.as_deref().unwrap_or("unknown completion error")
                    );
                }
                completed.insert(session);
            }

            if completed.len() == expected.len() {
                return tracked_entries.len() as u64;
            }
            if tracked_entries.len() > last_observed_count || completed.len() > last_completed_count
            {
                last_progress = Instant::now();
                last_observed_count = tracked_entries.len();
                last_completed_count = completed.len();
            }
            if start.elapsed() >= DAEMON_TEST_SYNC_TOTAL_TIMEOUT
                || last_progress.elapsed() >= DAEMON_TEST_SYNC_IDLE_TIMEOUT
            {
                break;
            }

            thread::sleep(Duration::from_millis(10));
        }

        panic!(
            "daemon completion log for family {} did not observe all sessions within timeout: {:?}",
            family_key, sessions
        );
    }

    pub(crate) fn wait_for_daemon_total_completion_count(
        &self,
        baseline_count: u64,
        expected_count: u64,
    ) -> u64 {
        let family_key = self.daemon_family_key();
        let start = Instant::now();
        let mut last_progress = start;
        let mut last_observed_count = baseline_count;
        loop {
            let entries = self.daemon_completion_entries_for_family(&family_key);
            if let Some(error_entry) = entries
                .iter()
                .skip(baseline_count as usize)
                .find(|entry| entry.status == "error")
            {
                panic!(
                    "daemon completion log reported an error for family {}: {}",
                    family_key,
                    error_entry
                        .error
                        .as_deref()
                        .unwrap_or("unknown completion error")
                );
            }
            let observed_count = entries.len() as u64;
            if observed_count >= expected_count {
                return observed_count;
            }
            if observed_count > last_observed_count {
                last_progress = Instant::now();
                last_observed_count = observed_count;
            }
            if start.elapsed() >= DAEMON_TEST_SYNC_TOTAL_TIMEOUT
                || last_progress.elapsed() >= DAEMON_TEST_SYNC_IDLE_TIMEOUT
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let final_entries = self.daemon_completion_entries_for_family(&family_key);
        let observed_count = final_entries.len() as u64;
        let recent_entries = final_entries
            .iter()
            .rev()
            .take(5)
            .map(|entry| format!("{}:{:?}:{}", entry.seq, entry.primary_command, entry.status))
            .collect::<Vec<_>>();

        panic!(
            "daemon completion log for family {} did not reach {} total entries within timeout (observed {}, recent entries {:?})",
            family_key, expected_count, observed_count, recent_entries
        );
    }

    pub(crate) fn wait_for_next_daemon_checkpoint_completion(&self, baseline_count: u64) -> u64 {
        self.wait_for_daemon_total_completion_count(
            baseline_count,
            baseline_count.saturating_add(1),
        )
    }

    fn daemon_family_key_for_repo_path(&self, repo_path: &Path) -> String {
        let repo = GitAiRepository::find_repository_in_path(repo_path.to_str().unwrap())
            .unwrap_or_else(|e| {
                panic!(
                    "failed to resolve daemon family key for {}: {}",
                    repo_path.display(),
                    e
                )
            });
        let common_dir = repo
            .common_dir()
            .canonicalize()
            .unwrap_or_else(|_| repo.common_dir().to_path_buf());
        common_dir.to_string_lossy().to_string()
    }

    fn maybe_daemon_family_key_for_repo_path(&self, repo_path: &Path) -> Option<String> {
        let lookup_path = if repo_path.is_dir() {
            repo_path.to_path_buf()
        } else {
            repo_path.parent()?.to_path_buf()
        };
        let repo = GitAiRepository::find_repository_in_path(lookup_path.to_str()?).ok()?;
        let common_dir = repo
            .common_dir()
            .canonicalize()
            .unwrap_or_else(|_| repo.common_dir().to_path_buf());
        Some(common_dir.to_string_lossy().to_string())
    }

    fn daemon_family_key(&self) -> String {
        self.daemon_family_key
            .get_or_init(|| self.daemon_family_key_for_repo_path(&self.path))
            .clone()
    }

    pub(crate) fn record_daemon_family_expected_completion_session(&self, session: &str) {
        if !self.git_mode.uses_daemon() {
            return;
        }

        let family_key = self.daemon_family_key();
        let mut registry = daemon_sync_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.record_expected_completion_session(&family_key, session);
    }

    pub(crate) fn append_daemon_test_sync_session_args(
        &self,
        args: &mut Vec<String>,
        session: &str,
    ) {
        if !self.git_mode.uses_daemon() {
            return;
        }

        args.push("-c".to_string());
        args.push(format!(
            "{}={}",
            git_ai::daemon::test_sync::TEST_SYNC_SESSION_CONFIG_KEY,
            session
        ));
    }

    fn checkpoint_path_args<'a>(&self, args: &'a [&'a str]) -> Vec<&'a str> {
        if git_ai_primary_command(args) != Some("checkpoint") {
            return Vec::new();
        }

        let mut candidates = Vec::new();
        let mut i = 1usize;
        let mut seen_separator = false;
        while i < args.len() {
            let arg = args[i];
            if seen_separator {
                candidates.push(arg);
                i += 1;
                continue;
            }

            match arg {
                "--" => {
                    seen_separator = true;
                    i += 1;
                }
                "--hook-input" => {
                    i += 2;
                }
                "--reset" => {
                    i += 1;
                }
                _ if arg.starts_with("--hook-input=") || arg.starts_with('-') => {
                    i += 1;
                }
                _ if i == 1 && is_known_checkpoint_preset(arg) => {
                    i += 1;
                }
                _ => {
                    candidates.push(arg);
                    i += 1;
                }
            }
        }

        candidates
    }

    pub(crate) fn sync_daemon_force(&self) {
        if !self.git_mode.uses_daemon() {
            return;
        }

        let family_key = self.daemon_family_key();
        self.sync_pending_daemon_sessions(&family_key);
    }

    pub(crate) fn sync_daemon_external_completion_sessions(&self, sessions: &[String]) {
        if !self.git_mode.uses_daemon() || sessions.is_empty() {
            return;
        }

        for session in sessions {
            self.record_daemon_family_expected_completion_session(session);
        }
        self.sync_daemon_force();
    }

    fn sync_daemon_clone_target(&self, target_repo_path: &Path) {
        if !self.git_mode.uses_daemon() {
            return;
        }

        let family_key = self.daemon_family_key_for_repo_path(target_repo_path);
        let baseline_count = {
            let registry = daemon_sync_registry()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            registry.last_synced_completion_count(&family_key)
        };
        let observed_count = self.wait_for_daemon_completion_count(
            &family_key,
            baseline_count,
            baseline_count.saturating_add(1),
        );
        let mut registry = daemon_sync_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.mark_synced_through(&family_key, observed_count);
    }

    fn sync_pending_daemon_sessions(&self, family_key: &str) {
        let pending_sessions = {
            let registry = daemon_sync_registry()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            registry.pending_sessions(family_key)
        };

        if pending_sessions.is_empty() {
            return;
        }

        let observed_count =
            self.wait_for_daemon_completion_sessions(family_key, &pending_sessions);
        let mut registry = daemon_sync_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.clear_pending_sessions(family_key);
        registry.advance_last_synced_completion_count(family_key, observed_count);
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
        // Isolate all git + git-ai config reads from developer machine settings.
        configure_test_home_env(command, &self.test_home);

        if self.git_mode.uses_daemon() {
            command.env(
                "GIT_TRACE2_EVENT",
                DaemonConfig::trace2_event_target_for_path(&self.daemon_trace_socket_path()),
            );
            command.env("GIT_TRACE2_EVENT_NESTING", Self::trace2_nesting_value());
        }

        if self.git_mode.uses_hooks() {
            command.env("GIT_AI_GLOBAL_GIT_HOOKS", "true");
        }

        if self.git_mode.uses_wrapper() {
            command.env("GIT_AI", "git");
        }

        // In WrapperDaemon mode, the wrapper needs the daemon socket paths
        // to initialize the telemetry handle and send wrapper state.
        if self.git_mode == GitTestMode::WrapperDaemon {
            command.env("GIT_AI_DAEMON_HOME", self.daemon_home_path());
            command.env(
                "GIT_AI_DAEMON_CONTROL_SOCKET",
                self.daemon_control_socket_path(),
            );
            command.env(
                "GIT_AI_DAEMON_TRACE_SOCKET",
                self.daemon_trace_socket_path(),
            );
        }
    }

    fn configure_git_ai_env(&self, command: &mut Command) {
        // Isolate all git + git-ai config reads from developer machine settings.
        configure_test_home_env(command, &self.test_home);
        command.env("GIT_AI_DAEMON_HOME", self.daemon_home_path());
        command.env(
            "GIT_AI_DAEMON_CONTROL_SOCKET",
            self.daemon_control_socket_path(),
        );
        command.env(
            "GIT_AI_DAEMON_TRACE_SOCKET",
            self.daemon_trace_socket_path(),
        );

        if self.git_mode.uses_daemon() {
            command.env("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true");
        }

        if self.git_mode.uses_hooks() {
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

    pub fn mode(&self) -> GitTestMode {
        self.git_mode
    }

    pub fn sync_daemon(&self) {
        self.sync_daemon_force();
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
        self.git_og_with_env(args, &[])
    }

    /// Run a raw git command (bypassing git-ai hooks) with custom environment variables.
    /// Useful for creating commits with specific author/committer identities.
    pub fn git_og_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String> {
        #[cfg(windows)]
        let null_hooks = "NUL";
        #[cfg(not(windows))]
        let null_hooks = "/dev/null";

        let retry_limit = 8usize;
        let retry_delay = Duration::from_millis(50);
        let tracked_invocation =
            self.parsed_git_invocation_for_tracking(args, Some(self.path.as_path()));
        let command_affects_daemon = env_explicitly_enables_trace2(envs)
            && git_ai::daemon::test_sync::tracks_parsed_git_invocation_for_test_sync(
                &tracked_invocation,
            );
        for attempt in 0..=retry_limit {
            let daemon_command_pending = env_explicitly_enables_trace2(envs)
                && command_affects_daemon
                && !git_invocation_routes_to_clone_target(&tracked_invocation);
            let daemon_test_sync_session =
                daemon_command_pending.then(new_daemon_test_sync_session_id);

            let mut command = Command::new(real_git_executable());
            let mut command_args = vec!["-C".to_string(), self.path.to_str().unwrap().to_string()];
            command_args.push("-c".to_string());
            command_args.push(format!("core.hooksPath={}", null_hooks));
            if let Some(session) = daemon_test_sync_session.as_deref() {
                self.append_daemon_test_sync_session_args(&mut command_args, session);
            }
            command_args.extend(args.iter().map(|s| s.to_string()));
            command.args(&command_args);
            configure_test_home_env(&mut command, &self.test_home);
            for (key, value) in envs {
                command.env(key, value);
            }

            let output = command
                .output()
                .unwrap_or_else(|_| panic!("Failed to execute git_og command: {:?}", args));

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
                if command_affects_daemon
                    && git_invocation_routes_to_clone_target(&tracked_invocation)
                {
                    let clone_cwd = self.path.as_path();
                    if let Some(target_repo_path) = clone_target_path(args, clone_cwd) {
                        self.sync_daemon_clone_target(&target_repo_path);
                    }
                } else if daemon_command_pending {
                    self.record_daemon_family_expected_completion_session(
                        daemon_test_sync_session
                            .as_deref()
                            .expect("daemon test sync session should exist for tracked command"),
                    );
                }
                return Ok(combined);
            }

            if attempt < retry_limit && is_transient_git_index_lock_error(&stderr) {
                std::thread::sleep(retry_delay);
                continue;
            }

            if daemon_command_pending {
                self.record_daemon_family_expected_completion_session(
                    daemon_test_sync_session
                        .as_deref()
                        .expect("daemon test sync session should exist for tracked command"),
                );
            }
            return Err(format!("{}{}", stdout, stderr));
        }

        Err("git_og_with_env failed after retries".to_string())
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
        let canonical_working_dir = if let Some(working_dir_path) = working_dir {
            Some(working_dir_path.canonicalize().map_err(|e| {
                format!(
                    "Failed to canonicalize working directory {}: {}",
                    working_dir_path.display(),
                    e
                )
            })?)
        } else {
            None
        };

        let command_context = canonical_working_dir
            .as_deref()
            .or(Some(self.path.as_path()));
        let tracked_invocation = self.parsed_git_invocation_for_tracking(args, command_context);

        if git_invocation_requires_daemon_sync(&tracked_invocation) {
            self.sync_daemon_force();
        }

        let retry_limit = 8usize;
        let retry_delay = Duration::from_millis(50);
        let command_affects_daemon = self.git_mode.uses_daemon()
            && git_ai::daemon::test_sync::tracks_parsed_git_invocation_for_test_sync(
                &tracked_invocation,
            );
        for attempt in 0..=retry_limit {
            let daemon_command_pending = self.git_mode.uses_daemon()
                && command_affects_daemon
                && !git_invocation_routes_to_clone_target(&tracked_invocation);
            let daemon_test_sync_session =
                daemon_command_pending.then(new_daemon_test_sync_session_id);

            let mut command = if self.git_mode.uses_wrapper() {
                Command::new(get_binary_path())
            } else {
                Command::new(real_git_executable())
            };

            // If working_dir is provided, use current_dir instead of -C flag
            // This tests that git-ai correctly finds the repository root when run from a subdirectory
            // The working_dir will be canonicalized to ensure it's an absolute path
            let mut command_args = Vec::<String>::new();
            if let Some(session) = daemon_test_sync_session.as_deref() {
                self.append_daemon_test_sync_session_args(&mut command_args, session);
            }
            if let Some(absolute_working_dir) = canonical_working_dir.as_ref() {
                command_args.extend(args.iter().map(|arg| (*arg).to_string()));
                command
                    .args(&command_args)
                    .current_dir(absolute_working_dir);
            } else {
                command_args.push("-C".to_string());
                command_args.push(self.path.to_str().unwrap().to_string());
                command_args.extend(args.iter().map(|arg| (*arg).to_string()));
                command.args(&command_args);
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
                if command_affects_daemon {
                    if self.git_mode.uses_daemon()
                        && git_invocation_routes_to_clone_target(&tracked_invocation)
                    {
                        let clone_cwd = canonical_working_dir
                            .as_deref()
                            .unwrap_or(self.path.as_path());
                        if let Some(target_repo_path) = clone_target_path(args, clone_cwd) {
                            self.sync_daemon_clone_target(&target_repo_path);
                        }
                    } else if daemon_command_pending {
                        self.record_daemon_family_expected_completion_session(
                            daemon_test_sync_session.as_deref().expect(
                                "daemon test sync session should exist for tracked command",
                            ),
                        );
                    }
                }
                return Ok(combined);
            }

            if attempt < retry_limit && is_transient_git_index_lock_error(&stderr) {
                std::thread::sleep(retry_delay);
                continue;
            }

            if daemon_command_pending {
                self.record_daemon_family_expected_completion_session(
                    daemon_test_sync_session
                        .as_deref()
                        .expect("daemon test sync session should exist for tracked command"),
                );
            }
            return Err(stderr);
        }

        Err("git_with_env failed after retries".to_string())
    }

    pub fn git_ai_from_working_dir(
        &self,
        working_dir: &std::path::Path,
        args: &[&str],
    ) -> Result<String, String> {
        if git_ai_command_requires_daemon_sync(args) {
            self.sync_daemon_force();
        }

        let binary_path = get_binary_path();

        let mut command = Command::new(binary_path);
        let normalized_args = normalize_test_git_ai_checkpoint_args(args);

        let absolute_working_dir = working_dir.canonicalize().map_err(|e| {
            format!(
                "Failed to canonicalize working directory {}: {}",
                working_dir.display(),
                e
            )
        })?;
        command
            .args(&normalized_args)
            .current_dir(&absolute_working_dir);
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
        if git_ai_command_requires_daemon_sync(args) {
            self.sync_daemon_force();
        }

        let binary_path = get_binary_path();
        let normalized_args = normalize_test_git_ai_checkpoint_args(args);

        let mut command = Command::new(binary_path);
        command.args(&normalized_args).current_dir(&self.path);
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

        if git_ai_command_requires_daemon_sync(args) {
            self.sync_daemon_force();
        }

        let binary_path = get_binary_path();
        let normalized_args = normalize_test_git_ai_checkpoint_args(args);

        let mut command = Command::new(binary_path);
        command
            .args(&normalized_args)
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
        self.sync_daemon_force();

        let repo = GitAiRepository::find_repository_in_path(self.path.to_str().unwrap())
            .expect("Failed to find repository");

        // Get the current HEAD commit SHA, or use "initial" for empty repos
        let commit_sha = repo
            .head()
            .ok()
            .and_then(|head| head.target().ok())
            .unwrap_or_else(|| "initial".to_string());

        // Get the working log for the current HEAD commit
        repo.storage
            .working_log_for_base_commit(&commit_sha)
            .unwrap()
    }

    pub fn read_authorship_note(&self, commit_sha: &str) -> Option<String> {
        self.git(&["notes", "--ref=ai", "show", commit_sha])
            .ok()
            .filter(|note| !note.trim().is_empty())
    }

    pub fn read_authorship_note_in_git_dir(
        &self,
        git_dir: &Path,
        commit_sha: &str,
    ) -> Option<String> {
        self.sync_daemon_force();

        let mut command = Command::new(real_git_executable());
        configure_test_home_env(&mut command, &self.test_home);
        command.args([
            "--git-dir",
            git_dir.to_str().expect("valid git dir"),
            "--no-pager",
            "notes",
            "--ref=ai",
            "show",
            commit_sha,
        ]);

        let output = command
            .output()
            .expect("failed to run git notes show in git dir");

        if !output.status.success() {
            return None;
        }

        let note = String::from_utf8_lossy(&output.stdout).to_string();
        if note.trim().is_empty() {
            None
        } else {
            Some(note)
        }
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

    pub fn stage_all_and_commit_with_env(
        &self,
        message: &str,
        envs: &[(&str, &str)],
    ) -> Result<NewCommit, String> {
        self.git(&["add", "-A"]).expect("add --all should succeed");
        self.commit_with_env(message, envs, None)
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

                self.sync_daemon_force();

                let content = git_ai::git::refs::show_authorship_note(&repo, &head_commit)
                    .ok_or_else(|| {
                        format!(
                            "No authorship log found for new commit {} after daemon sync",
                            head_commit
                        )
                    })?;
                let authorship_log = AuthorshipLog::deserialize_from_string(&content)
                    .map_err(|e| format!("Failed to parse authorship log: {}", e))?;

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
        if std::env::var("GIT_AI_TEST_KEEP_REPOS")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            return;
        }

        if self.daemon_scope == DaemonTestScope::Dedicated
            && let Some(daemon) = self.daemon_process.take()
        {
            daemon.shutdown();
        }

        let remove_test_db =
            !(self.git_mode.uses_daemon() && self.daemon_scope == DaemonTestScope::Shared);

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

            if let Some(base_db_path) = &self._base_test_db_path
                && remove_test_db
            {
                let _ = remove_dir_all_with_retry(base_db_path, 40, Duration::from_millis(25));
            }

            if remove_test_db {
                let _ =
                    remove_dir_all_with_retry(&self.test_db_path, 40, Duration::from_millis(25));
            }
            let _ = remove_dir_all_with_retry(&self.test_home, 40, Duration::from_millis(25));
            return;
        }

        remove_dir_all_with_retry(&self.path, 80, Duration::from_millis(50))
            .expect("failed to remove test repo");
        // Also clean up the test database directory (may not exist if no DB operations were done)
        if remove_test_db {
            let _ = remove_dir_all_with_retry(&self.test_db_path, 40, Duration::from_millis(25));
        }
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

#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error()
        .raw_os_error()
        .is_some_and(|code| code == libc::EPERM)
}

#[cfg(unix)]
fn reap_child_if_exited(pid: u32) -> bool {
    let mut status: libc::c_int = 0;
    let rc = unsafe {
        libc::waitpid(
            pid as libc::pid_t,
            &mut status as *mut libc::c_int,
            libc::WNOHANG,
        )
    };
    rc == pid as libc::pid_t || rc == -1
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

fn is_transient_git_index_lock_error(stderr: &str) -> bool {
    stderr.contains(".git/index.lock")
        && (stderr.contains("File exists")
            || stderr.contains("Another git process seems to be running"))
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

static DEFAULT_BRANCH_NAME: OnceLock<String> = OnceLock::new();
static TEMPLATE_REPO: OnceLock<PathBuf> = OnceLock::new();
static TEMPLATE_BARE_REPO: OnceLock<PathBuf> = OnceLock::new();
static COMPILED_BINARY: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn real_git_executable() -> &'static str {
    git_ai::config::Config::get().git_cmd()
}

/// Create a pre-initialized template repo (cached across all tests in the process).
/// Subsequent calls to `clone_template_to()` copy this instead of running git init.
fn init_template_repo() -> PathBuf {
    let path = std::env::temp_dir().join(format!("git-ai-test-template-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);

    let p = path.to_str().unwrap();
    let git = real_git_executable();

    let output = Command::new(git)
        .args(["init", p])
        .output()
        .expect("failed to init template repo");
    assert!(output.status.success(), "template git init failed");

    for args in [
        vec!["-C", p, "config", "user.name", "Test User"],
        vec!["-C", p, "config", "user.email", "test@example.com"],
        vec!["-C", p, "symbolic-ref", "HEAD", "refs/heads/main"],
    ] {
        let output = Command::new(git)
            .args(&args)
            .output()
            .expect("failed to configure template repo");
        assert!(
            output.status.success(),
            "template config failed: {:?}",
            args
        );
    }

    path
}

fn init_bare_template_repo() -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("git-ai-test-template-bare-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);

    let p = path.to_str().unwrap();
    let git = real_git_executable();

    let output = Command::new(git)
        .args(["init", "--bare", p])
        .output()
        .expect("failed to init bare template repo");
    assert!(output.status.success(), "bare template git init failed");

    let output = Command::new(git)
        .args(["-C", p, "symbolic-ref", "HEAD", "refs/heads/main"])
        .output()
        .expect("failed to set HEAD in bare template");
    assert!(output.status.success());

    path
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

/// Clone the cached template repo to a new destination path.
fn clone_template_to(dest: &std::path::Path) {
    let template = TEMPLATE_REPO.get_or_init(init_template_repo);
    copy_dir_recursive(template, dest).expect("failed to copy template repo");
}

/// Clone the cached bare template repo to a new destination path.
fn clone_bare_template_to(dest: &std::path::Path) {
    let template = TEMPLATE_BARE_REPO.get_or_init(init_bare_template_repo);
    copy_dir_recursive(template, dest).expect("failed to copy bare template repo");
}

/// Set user.name and user.email on a repo using git CLI (no git2 needed).
fn set_repo_user_config(repo_path: &std::path::Path) {
    let p = repo_path.to_str().unwrap();
    let git = real_git_executable();
    for args in [
        vec!["-C", p, "config", "user.name", "Test User"],
        vec!["-C", p, "config", "user.email", "test@example.com"],
    ] {
        let output = Command::new(git)
            .args(&args)
            .output()
            .expect("failed to set user config");
        assert!(output.status.success());
    }
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
    if let Ok(override_path) = std::env::var("GIT_AI_TEST_BINARY_PATH") {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            println!(
                "Using prebuilt git-ai test binary from GIT_AI_TEST_BINARY_PATH: {}",
                path.display()
            );
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

    // Respect CARGO_TARGET_DIR if set, otherwise fall back to manifest-relative target/
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

    // Warm the freshly built binary once so the first daemon startups in highly parallel
    // suites don't all pay cold process initialization overhead at the same time.
    let _ = Command::new(&binary_path).arg("--version").output();

    binary_path
}

pub fn get_binary_path() -> &'static PathBuf {
    COMPILED_BINARY.get_or_init(compile_binary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_test_git_ai_checkpoint_args_inserts_separator_for_direct_file() {
        assert_eq!(
            normalize_test_git_ai_checkpoint_args(&["checkpoint", "src/lib.rs"]),
            vec!["checkpoint", "--", "src/lib.rs"]
        );
        assert_eq!(
            normalize_test_git_ai_checkpoint_args(&["checkpoint", "--reset", "src/lib.rs"]),
            vec!["checkpoint", "--reset", "--", "src/lib.rs"]
        );
    }

    #[test]
    fn test_normalize_test_git_ai_checkpoint_args_preserves_known_presets_and_separator() {
        assert_eq!(
            normalize_test_git_ai_checkpoint_args(&["checkpoint", "mock_ai", "src/lib.rs"]),
            vec!["checkpoint", "mock_ai", "src/lib.rs"]
        );
        assert_eq!(
            normalize_test_git_ai_checkpoint_args(&["checkpoint", "--", "src/lib.rs"]),
            vec!["checkpoint", "--", "src/lib.rs"]
        );
    }

    #[test]
    fn test_normalize_test_git_ai_checkpoint_args_handles_hook_input_before_pathspecs() {
        assert_eq!(
            normalize_test_git_ai_checkpoint_args(&[
                "checkpoint",
                "--hook-input",
                "{\"cwd\":\"/tmp/repo\"}",
                "src/lib.rs",
                "src/main.rs",
            ]),
            vec![
                "checkpoint",
                "--hook-input",
                "{\"cwd\":\"/tmp/repo\"}",
                "--",
                "src/lib.rs",
                "src/main.rs",
            ]
        );
    }
}
