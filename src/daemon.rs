use crate::config;
use crate::daemon::domain::RepoContext;
use crate::daemon::git_backend::GitBackend;
use crate::error::GitAiError;
use crate::git::cli_parser::{
    ParsedGitInvocation, explicit_rebase_branch_arg, parse_git_cli_args,
    stash_requires_target_resolution, stash_target_spec, summarize_rebase_args,
};
use crate::git::find_repository_in_path;
use crate::git::repo_state::{
    HeadState, common_dir_for_worktree, git_dir_for_worktree, latest_reflog_old_oid_for_worktree,
    read_head_state_for_worktree, read_ref_oid_for_worktree, resolve_rebase_segment_for_worktree,
    resolve_reflog_old_oid_for_ref_new_oid_in_worktree, resolve_squash_source_head_for_worktree,
    resolve_stash_target_oid_for_worktree, resolve_worktree_head_reflog_old_oid_for_new_head,
    worktree_root_for_path,
};
use crate::git::repository::{Repository, discover_repository_in_path_no_git_exec, exec_git};
use crate::git::sync_authorship::{fetch_authorship_notes, fetch_remote_from_args};
use crate::utils::LockFile;
use crate::{
    authorship::virtual_attribution::{
        restore_virtual_attribution_carryover, restore_working_log_carryover,
    },
    authorship::working_log::CheckpointKind,
    commands::checkpoint_agent::orchestrator::CheckpointRequest,
    daemon::checkpoint::PreparedPathRole,
};
#[cfg(not(windows))]
use interprocess::local_socket::ConnectOptions;
#[cfg(not(windows))]
use interprocess::{
    ConnectWaitMode,
    local_socket::{GenericFilePath, ListenerOptions, Name, prelude::*},
};
#[cfg(windows)]
use named_pipe::{
    ConnectingServer as WindowsConnectingServer, OpenMode as WindowsPipeOpenMode,
    PipeClient as WindowsPipeClient, PipeOptions as WindowsPipeOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
#[cfg(windows)]
use std::io;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc, oneshot};
use tokio::time::Duration;

pub mod analyzers;
pub mod bash_sessions;
pub mod checkpoint;
pub mod control_api;
pub mod coordinator;
pub mod domain;
pub mod family_actor;
pub mod git_backend;
pub mod global_actor;
pub mod reducer;
pub mod sentry_layer;
pub mod sweep_coordinator;
pub mod telemetry_handle;
pub mod telemetry_worker;
pub mod test_sync;
pub mod trace_normalizer;
pub mod transcript_redaction;
pub mod transcript_worker;

pub use control_api::{
    BashSessionQueryResponse, BashSnapshotQueryResponse, ControlRequest, ControlResponse,
    FamilyStatus, TelemetryEnvelope,
};

const PID_META_FILE: &str = "daemon.pid.json";
const TRACE_INGEST_SEQ_FIELD: &str = "git_ai_ingest_seq";
const DAEMON_CONTROL_CONNECT_TIMEOUT: Duration = Duration::from_millis(250);
const DAEMON_CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
const DAEMON_CHECKPOINT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(300);
const DAEMON_SOCKET_PROBE_TIMEOUT: Duration = Duration::from_millis(100);
#[cfg(windows)]
const WINDOWS_TRACE_PIPE_WORKERS: usize = 16;
#[cfg(windows)]
const WINDOWS_CONTROL_PIPE_WORKERS: usize = 8;
#[cfg(windows)]
const WINDOWS_STDOUT_HANDLE: u32 = (-11i32) as u32;
#[cfg(windows)]
const WINDOWS_STDERR_HANDLE: u32 = (-12i32) as u32;
static DAEMON_PROCESS_ACTIVE: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
unsafe extern "system" {
    fn SetStdHandle(nstdhandle: u32, hhandle: *mut std::ffi::c_void) -> i32;
}

#[cfg(not(windows))]
pub type DaemonClientStream = LocalSocketStream;

#[cfg(windows)]
pub enum DaemonClientStream {
    WindowsPipe(WindowsPipeClient),
}

#[cfg(windows)]
impl Read for DaemonClientStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::WindowsPipe(stream) => stream.read(buf),
        }
    }
}

#[cfg(windows)]
impl Write for DaemonClientStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::WindowsPipe(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::WindowsPipe(stream) => stream.flush(),
        }
    }
}

pub fn daemon_process_active() -> bool {
    DAEMON_PROCESS_ACTIVE.load(Ordering::SeqCst)
}

struct DaemonProcessActiveGuard;

impl DaemonProcessActiveGuard {
    fn enter() -> Self {
        DAEMON_PROCESS_ACTIVE.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for DaemonProcessActiveGuard {
    fn drop(&mut self) {
        DAEMON_PROCESS_ACTIVE.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub internal_dir: PathBuf,
    pub lock_path: PathBuf,
    pub trace_socket_path: PathBuf,
    pub control_socket_path: PathBuf,
}

impl DaemonConfig {
    fn from_internal_dir(internal_dir: PathBuf) -> Self {
        let daemon_dir = internal_dir.join("daemon");
        #[cfg(unix)]
        let (lock_path, trace_socket_path, control_socket_path) = {
            let mut lock_path = daemon_dir.join("daemon.lock");
            let mut trace_socket_path = daemon_dir.join("trace2.sock");
            let mut control_socket_path = daemon_dir.join("control.sock");
            let too_long = |path: &Path| path.to_string_lossy().len() >= 100;

            if too_long(&trace_socket_path) || too_long(&control_socket_path) {
                let mut hasher = Sha256::new();
                hasher.update(internal_dir.to_string_lossy().as_bytes());
                let digest = format!("{:x}", hasher.finalize());
                let short = &digest[..16];
                let short_dir = std::env::temp_dir().join(format!("git-ai-d-{}", short));
                lock_path = short_dir.join("daemon.lock");
                trace_socket_path = short_dir.join("trace.sock");
                control_socket_path = short_dir.join("control.sock");
            }

            (lock_path, trace_socket_path, control_socket_path)
        };

        #[cfg(not(unix))]
        let (lock_path, trace_socket_path, control_socket_path) = {
            let mut hasher = Sha256::new();
            hasher.update(internal_dir.to_string_lossy().as_bytes());
            let digest = format!("{:x}", hasher.finalize());
            let short = &digest[..16];
            (
                daemon_dir.join("daemon.lock"),
                PathBuf::from(format!(r"\\.\pipe\git-ai-{}-trace2", short)),
                PathBuf::from(format!(r"\\.\pipe\git-ai-{}-control", short)),
            )
        };

        Self {
            internal_dir,
            lock_path,
            trace_socket_path,
            control_socket_path,
        }
    }

    pub fn from_home(home: &Path) -> Self {
        let internal_dir = home.join(".git-ai").join("internal");
        Self::from_internal_dir(internal_dir)
    }

    pub fn from_default_paths() -> Result<Self, GitAiError> {
        let internal_dir = config::internal_dir_path().ok_or_else(|| {
            GitAiError::Generic("Unable to determine ~/.git-ai/internal path".to_string())
        })?;
        Ok(Self::from_internal_dir(internal_dir))
    }

    pub fn from_env_or_default_paths() -> Result<Self, GitAiError> {
        let mut config = if let Ok(home) = std::env::var("GIT_AI_DAEMON_HOME")
            && !home.trim().is_empty()
        {
            Self::from_home(Path::new(&home))
        } else {
            Self::from_default_paths()?
        };

        if let Ok(path) = std::env::var("GIT_AI_DAEMON_CONTROL_SOCKET")
            && !path.trim().is_empty()
        {
            config.control_socket_path = PathBuf::from(path);
        }

        if let Ok(path) = std::env::var("GIT_AI_DAEMON_TRACE_SOCKET")
            && !path.trim().is_empty()
        {
            config.trace_socket_path = PathBuf::from(path);
        }

        Ok(config)
    }

    pub fn ensure_parent_dirs(&self) -> Result<(), GitAiError> {
        let daemon_dir = self
            .lock_path
            .parent()
            .ok_or_else(|| GitAiError::Generic("daemon lock path has no parent".to_string()))?;
        fs::create_dir_all(daemon_dir)?;
        fs::create_dir_all(&self.internal_dir)?;
        Ok(())
    }

    pub fn trace2_event_target(&self) -> String {
        Self::trace2_event_target_for_path(&self.trace_socket_path)
    }

    pub fn test_completion_log_dir(&self) -> PathBuf {
        self.internal_dir.join("daemon").join("test-completions")
    }

    pub fn test_completion_log_path_for_family(&self, family_key: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(family_key.as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        self.test_completion_log_dir()
            .join(format!("{}.jsonl", &digest[..16]))
    }

    pub fn trace2_event_target_for_path(path: &Path) -> String {
        #[cfg(unix)]
        {
            format!("af_unix:stream:{}", path.to_string_lossy())
        }
        #[cfg(not(unix))]
        {
            path.to_string_lossy().to_string()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonPidMeta {
    pid: u32,
    started_at_ns: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TestCompletionLogEntry {
    seq: u64,
    family_key: String,
    kind: String,
    primary_command: Option<String>,
    #[serde(default)]
    test_sync_session: Option<String>,
    exit_code: Option<i32>,
    #[serde(default)]
    sync_tracked: bool,
    status: String,
    error: Option<String>,
}

pub struct DaemonLock {
    _lock: LockFile,
}

impl DaemonLock {
    pub fn acquire(path: &Path) -> Result<Self, GitAiError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = LockFile::try_acquire(path).ok_or_else(|| {
            GitAiError::Generic(
                "git-ai background service is already running (lock held)".to_string(),
            )
        })?;
        Ok(Self { _lock: lock })
    }
}

fn is_trace_payload(payload: &Value) -> bool {
    payload.get("event").and_then(Value::as_str).is_some()
}

fn trace_root_sid(sid: &str) -> &str {
    sid.split('/').next().unwrap_or(sid)
}

fn is_terminal_root_trace_event(event: &str, sid: &str, root: &str) -> bool {
    sid == root && matches!(event, "exit" | "atexit")
}

fn daemon_worktree_from_repo_path(repo_path: &Path) -> Option<PathBuf> {
    if repo_path.file_name().and_then(|name| name.to_str()) == Some(".git") {
        return repo_path.parent().map(PathBuf::from);
    }

    let linked_gitdir_file = repo_path.join("gitdir");
    if linked_gitdir_file.is_file() {
        let content = fs::read_to_string(&linked_gitdir_file).ok()?;
        let linked = PathBuf::from(content.trim());
        if linked.file_name().and_then(|name| name.to_str()) == Some(".git") {
            return linked.parent().map(PathBuf::from);
        }
    }

    None
}

fn trace_payload_worktree_hint(payload: &Value) -> Option<PathBuf> {
    let normalize = |path: PathBuf| worktree_root_for_path(&path).unwrap_or(path);
    let argv = trace_payload_argv(payload);
    let event = payload
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if event == "def_repo" {
        if let Some(path) = payload
            .get("worktree")
            .or_else(|| payload.get("repo_working_dir"))
            .and_then(Value::as_str)
        {
            return Some(normalize(PathBuf::from(path)));
        }
        if let Some(repo_path) = payload.get("repo").and_then(Value::as_str) {
            let candidate = PathBuf::from(repo_path);
            if let Some(worktree) = daemon_worktree_from_repo_path(&candidate) {
                return Some(normalize(worktree));
            }
        }
    }
    if let Some(path) = payload.get("worktree").and_then(Value::as_str) {
        return Some(normalize(PathBuf::from(path)));
    }
    if let Some(cwd) = payload.get("cwd").and_then(Value::as_str)
        && let Some(base_dir) = trace_payload_command_base_dir(payload, &argv, Path::new(cwd))
    {
        return Some(normalize(base_dir));
    }
    let parsed = parse_git_cli_args(trace_invocation_args(&argv));
    let mut idx = 0usize;
    while idx < parsed.global_args.len() {
        let token = &parsed.global_args[idx];
        if token == "-C" {
            let path_arg = parsed.global_args.get(idx + 1)?;
            let candidate = PathBuf::from(path_arg);
            if candidate.is_absolute() {
                return Some(normalize(candidate));
            }
            return None;
        }
        if let Some(path_arg) = token.strip_prefix("-C")
            && !path_arg.is_empty()
        {
            let candidate = PathBuf::from(path_arg);
            if candidate.is_absolute() {
                return Some(normalize(candidate));
            }
            return None;
        }
        idx += 1;
    }
    if argv.is_empty() {
        return None;
    }
    None
}

fn trace_payload_command_base_dir(
    _payload: &Value,
    argv: &[String],
    cwd: &Path,
) -> Option<PathBuf> {
    let parsed = parse_git_cli_args(trace_invocation_args(argv));
    let mut base = cwd.to_path_buf();
    let mut idx = 0usize;

    while idx < parsed.global_args.len() {
        let token = &parsed.global_args[idx];

        if token == "-C" {
            let path_arg = parsed.global_args.get(idx + 1)?;
            let next_base = PathBuf::from(path_arg);
            base = if next_base.is_absolute() {
                next_base
            } else {
                base.join(next_base)
            };
            idx += 2;
            continue;
        }

        if let Some(path_arg) = token.strip_prefix("-C") {
            let next_base = PathBuf::from(path_arg);
            base = if next_base.is_absolute() {
                next_base
            } else {
                base.join(next_base)
            };
            idx += 1;
            continue;
        }

        idx += 1;
    }

    Some(base)
}

fn trace_payload_time_ns(payload: &Value) -> Option<u128> {
    payload
        .get("time")
        .and_then(Value::as_str)
        .and_then(rfc3339_to_unix_nanos)
        .or_else(|| {
            payload
                .get("time_ns")
                .and_then(Value::as_u64)
                .map(u128::from)
        })
        .or_else(|| payload.get("ts").and_then(Value::as_u64).map(u128::from))
        .or_else(|| {
            payload
                .get("t_abs")
                .and_then(Value::as_f64)
                .and_then(|seconds| {
                    if seconds.is_sign_negative() {
                        None
                    } else {
                        Some((seconds * 1_000_000_000_f64) as u128)
                    }
                })
        })
}

fn daemon_git_dir_for_worktree(worktree: &Path) -> Option<PathBuf> {
    git_dir_for_worktree(worktree)
}

fn daemon_worktree_head_reflog_offset(worktree: &Path) -> Option<u64> {
    let git_dir = daemon_git_dir_for_worktree(worktree)?;
    let path = git_dir.join("logs").join("HEAD");
    fs::metadata(path).ok().map(|metadata| metadata.len())
}

fn repo_context_from_head_state(state: HeadState) -> RepoContext {
    RepoContext {
        head: state.head,
        branch: state.branch,
        detached: state.detached,
    }
}

fn trace_payload_cmd_name(payload: &Value) -> Option<String> {
    payload
        .get("name")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn trace_payload_argv(payload: &Value) -> Vec<String> {
    payload
        .get("argv")
        .and_then(Value::as_array)
        .map(|argv| {
            argv.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn trace_payload_primary_command(payload: &Value) -> Option<String> {
    trace_payload_cmd_name(payload).or_else(|| {
        let argv = trace_payload_argv(payload);
        trace_argv_primary_command(&argv)
    })
}

fn trace_argv_primary_command(argv: &[String]) -> Option<String> {
    let mut idx = 0;
    if argv
        .first()
        .map(|token| {
            let file_name = Path::new(token)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(token);
            file_name == "git" || file_name == "git.exe"
        })
        .unwrap_or(false)
    {
        idx = 1;
    }
    while idx < argv.len() {
        let token = argv[idx].as_str();
        if token == "-C" {
            idx += 2;
            continue;
        }
        if matches!(
            token,
            "-c" | "--config-env"
                | "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--super-prefix"
                | "--exec-path"
                | "--worktree-attributes"
                | "--attr-source"
        ) {
            idx += 2;
            continue;
        }
        if token.starts_with("--") && token.contains('=') {
            idx += 1;
            continue;
        }
        if token.starts_with('-') {
            idx += 1;
            continue;
        }
        return Some(token.to_string());
    }
    None
}

/// Extract the subcommand from a trace2 argv after the primary command.
///
/// For an invocation like `git -c core.fsmonitor=false stash list` this
/// returns `Some("list")`.  Used together with the primary command to
/// identify read-only invocations such as `stash list` and `worktree list`
/// that would otherwise be misclassified as potentially-mutating.
fn trace_argv_subcommand(argv: &[String]) -> Option<String> {
    // Walk the argv twice:
    //   pass 1 — find the index of the primary command (same logic as
    //            trace_argv_primary_command)
    //   pass 2 — find the first non-flag token after that index
    let mut idx = 0;
    // Skip the git binary itself
    if argv
        .first()
        .map(|token| {
            let file_name = Path::new(token)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(token);
            file_name == "git" || file_name == "git.exe"
        })
        .unwrap_or(false)
    {
        idx = 1;
    }
    // Skip git global flags to reach the primary command
    let cmd_idx = loop {
        if idx >= argv.len() {
            return None;
        }
        let token = argv[idx].as_str();
        if token == "-C" {
            idx += 2;
            continue;
        }
        if matches!(
            token,
            "-c" | "--config-env"
                | "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--super-prefix"
                | "--exec-path"
                | "--worktree-attributes"
                | "--attr-source"
        ) {
            idx += 2;
            continue;
        }
        if token.starts_with("--") && token.contains('=') {
            idx += 1;
            continue;
        }
        if token.starts_with('-') {
            idx += 1;
            continue;
        }
        break idx;
    };
    // cmd_idx points at the primary command.  Advance past it and find the
    // first non-flag positional argument — the subcommand.
    let mut idx = cmd_idx + 1;
    while idx < argv.len() {
        let token = argv[idx].as_str();
        if token.starts_with('-') {
            idx += 1;
            continue;
        }
        return Some(token.to_string());
    }
    None
}

/// Returns true when the trace2 event's command+subcommand pair is
/// guaranteed to never mutate repository state.
///
/// This extends the simple command check to handle commands like `stash`
/// and `worktree` whose mutability depends on the subcommand (e.g.,
/// `git stash list` is read-only while `git stash pop` is not).
fn trace_invocation_is_definitely_read_only(
    primary_command: Option<&str>,
    argv: &[String],
) -> bool {
    use crate::git::command_classification::is_definitely_read_only_invocation;
    match primary_command {
        Some(cmd) => {
            // Only parse the subcommand for commands that need it; parsing is
            // cheap but this avoids it for the majority of clearly-read-only
            // commands like status, diff, show, etc.
            let subcommand = if matches!(cmd, "stash" | "worktree") {
                trace_argv_subcommand(argv)
            } else {
                None
            };
            is_definitely_read_only_invocation(cmd, subcommand.as_deref())
        }
        None => false,
    }
}

fn trace_command_may_mutate_refs(primary_command: Option<&str>) -> bool {
    matches!(
        primary_command,
        Some(
            "cherry-pick"
                | "checkout"
                | "clone"
                | "commit"
                | "fetch"
                | "init"
                | "merge"
                | "pull"
                | "push"
                | "rebase"
                | "reset"
                | "stash"
                | "switch"
        )
    )
}

fn trace_command_uses_target_repo_context_only(primary_command: Option<&str>) -> bool {
    matches!(primary_command, Some("clone" | "init"))
}

fn trace_invocation_args(argv: &[String]) -> &[String] {
    if argv
        .first()
        .map(|token| {
            Path::new(token)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "git" || name == "git.exe")
        })
        .unwrap_or(false)
    {
        &argv[1..]
    } else {
        argv
    }
}

fn matches_any_pathspec(file: &str, pathspecs: &[String]) -> bool {
    pathspecs.iter().any(|pathspec| {
        file == pathspec
            || (pathspec.ends_with('/') && file.starts_with(pathspec))
            || file.starts_with(&format!("{}/", pathspec))
    })
}

fn sync_pre_commit_checkpoint(
    repo: &Repository,
    base_commit: &str,
    carryover_snapshot: Option<&HashMap<String, String>>,
) -> Result<(), GitAiError> {
    if base_commit.trim().is_empty() {
        return Ok(());
    }
    if !repo.storage.has_working_log(base_commit) {
        return Ok(());
    }

    let working_log = repo.storage.working_log_for_base_commit(base_commit)?;
    let initial = working_log.read_initial_attributions();

    let dirty_files: HashMap<String, String> = if let Some(snapshot) = carryover_snapshot {
        snapshot.clone()
    } else {
        return Ok(());
    };

    let mut files_to_replay: Vec<String> = Vec::new();
    let mut replay_dirty: HashMap<String, String> = HashMap::new();

    for (file_path, target_content) in &dirty_files {
        let current_tracked = working_log.effective_tracked_file_content(&initial, file_path)?;
        let needs_replay = match current_tracked {
            None => true,
            Some(ref tracked) => tracked != target_content,
        };
        if needs_replay {
            files_to_replay.push(file_path.clone());
            replay_dirty.insert(file_path.clone(), target_content.clone());
        }
    }

    if files_to_replay.is_empty() {
        return Ok(());
    }
    files_to_replay.sort();

    let repo_workdir = repo
        .workdir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let repo_work_dir_path = std::path::PathBuf::from(&repo_workdir);

    let checkpoint_files: Vec<crate::commands::checkpoint_agent::orchestrator::CheckpointFile> =
        files_to_replay
            .iter()
            .map(
                |path| crate::commands::checkpoint_agent::orchestrator::CheckpointFile {
                    path: std::path::PathBuf::from(path),
                    content: replay_dirty.get(path).cloned(),
                    repo_work_dir: repo_work_dir_path.clone(),
                    base_commit:
                        crate::commands::checkpoint_agent::orchestrator::BaseCommit::Initial,
                },
            )
            .collect();

    let request = CheckpointRequest {
        trace_id: crate::authorship::authorship_log_serialization::generate_trace_id(),
        checkpoint_kind: CheckpointKind::Human,
        agent_id: None,
        files: checkpoint_files,
        path_role: crate::daemon::checkpoint::PreparedPathRole::WillEdit,
        transcript_source: None,
        metadata: std::collections::HashMap::new(),
    };

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let resolved = crate::daemon::checkpoint::ResolvedCheckpointExecution {
        base_commit: base_commit.to_string(),
        ts,
        files: files_to_replay,
        dirty_files: replay_dirty,
    };

    let author = repo.git_author_identity().formatted_or_unknown();
    crate::daemon::checkpoint::execute_resolved_checkpoint_from_daemon(
        repo,
        &author,
        CheckpointKind::Human,
        request,
        resolved,
    )
}

fn tracked_working_log_files(
    repo: &Repository,
    base_commit: &str,
) -> Result<HashSet<String>, GitAiError> {
    if base_commit.trim().is_empty() || !repo.storage.has_working_log(base_commit) {
        return Ok(HashSet::new());
    }

    let working_log = repo.storage.working_log_for_base_commit(base_commit)?;
    let initial = working_log.read_initial_attributions();
    let mut files: HashSet<String> = initial.files.keys().cloned().collect();
    files.extend(working_log.all_touched_files()?);
    Ok(files)
}

fn resolve_stash_sha(cmd: &crate::daemon::domain::NormalizedCommand) -> Option<&str> {
    cmd.stash_target_oid.as_deref().or_else(|| {
        cmd.ref_changes
            .iter()
            .find(|rc| rc.reference == "refs/stash")
            .map(|rc| rc.old.as_str())
            .filter(|s| !s.is_empty() && *s != "0000000000000000000000000000000000000000")
    })
}

/// After a rebase completes, check if any newly-rebased commits were created
/// from conflict resolution with AI checkpoints. If so, run post_commit on
/// those commits to incorporate the AI attribution from the working log.
fn process_conflict_resolution_working_logs(repo: &Repository, new_tip: &str, onto: Option<&str>) {
    let onto_sha = match onto {
        Some(s) if !s.is_empty() => s,
        _ => return,
    };

    // Walk rebased commits between onto and new_tip
    let mut args = repo.global_args_for_exec();
    args.extend([
        "log".to_string(),
        "--format=%H %P".to_string(),
        format!("{}..{}", onto_sha, new_tip),
    ]);
    let output = match crate::git::repository::exec_git(&args) {
        Ok(o) => o,
        Err(_) => return,
    };
    let log_output = String::from_utf8_lossy(&output.stdout);

    for line in log_output.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let commit_sha = parts[0];
        let parent_sha = parts[1]; // first parent

        if !repo.storage.has_working_log(parent_sha) {
            continue;
        }

        // There's a working log at this parent — conflict resolution happened here.
        // Run post_commit to generate the authorship note from the working log.
        let author = repo.git_author_identity().formatted_or_unknown();
        let _ = crate::authorship::post_commit::post_commit_with_final_state(
            repo,
            Some(parent_sha.to_string()),
            commit_sha.to_string(),
            author,
            true,
            None,
        );
    }
}

fn committed_file_snapshot_for_amend(
    repo: &Repository,
    old_head: &str,
    new_head: &str,
) -> Option<HashMap<String, String>> {
    let mut files = repo
        .list_commit_files(new_head, None)
        .ok()
        .unwrap_or_default();
    if old_head != "initial"
        && let Ok(parent_files) = repo.list_commit_files(old_head, None)
    {
        files.extend(parent_files);
    }
    if files.is_empty() {
        return None;
    }
    let mut snapshot = HashMap::new();
    for file_path in &files {
        if let Ok(content) = repo.get_file_content(file_path, new_head)
            && let Ok(text) = String::from_utf8(content)
        {
            snapshot.insert(file_path.clone(), text);
        }
    }
    if snapshot.is_empty() {
        None
    } else {
        Some(snapshot)
    }
}

fn system_time_to_unix_nanos(time: SystemTime) -> Option<u128> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
}

fn rfc3339_to_unix_nanos(value: &str) -> Option<u128> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|timestamp| u128::try_from(timestamp.timestamp_nanos_opt()?).ok())
}

fn read_worktree_snapshot_for_files_at_or_before(
    worktree: &Path,
    file_paths: &HashSet<String>,
    max_modified_ns: u128,
) -> HashMap<String, String> {
    let mut snapshot = HashMap::new();
    for file_path in file_paths {
        let absolute = worktree.join(file_path);
        let modified_after_cutoff = fs::metadata(&absolute)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(system_time_to_unix_nanos)
            .is_some_and(|modified_ns| modified_ns > max_modified_ns);
        if modified_after_cutoff {
            continue;
        }

        let content = match fs::read(&absolute) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Err(_) => String::new(),
        };
        snapshot.insert(file_path.clone(), content);
    }
    snapshot
}
fn ref_change_span(
    ref_changes: &[crate::daemon::domain::RefChange],
    predicate: impl Fn(&crate::daemon::domain::RefChange) -> bool,
) -> Option<(String, String)> {
    let matching = ref_changes
        .iter()
        .filter(|change| predicate(change) && change.old.trim() != change.new.trim())
        .collect::<Vec<_>>();
    let first = matching.first()?;
    let last = matching.last()?;
    Some((first.old.clone(), last.new.clone()))
}

fn stable_head_change_from_ref_changes(
    ref_changes: &[crate::daemon::domain::RefChange],
) -> Option<(String, String)> {
    ref_change_span(ref_changes, |change| change.reference == "HEAD")
        .or_else(|| {
            ref_change_span(ref_changes, |change| {
                change.reference.starts_with("refs/heads/")
            })
        })
        .or_else(|| {
            ref_change_span(ref_changes, |change| {
                is_non_auxiliary_ref(&change.reference)
            })
        })
}

fn stable_new_head_from_ref_changes(
    ref_changes: &[crate::daemon::domain::RefChange],
) -> Option<String> {
    stable_head_change_from_ref_changes(ref_changes).map(|(_, new_head)| new_head)
}

fn stable_old_head_from_worktree_head_reflog(worktree: &Path, new_head: &str) -> Option<String> {
    resolve_worktree_head_reflog_old_oid_for_new_head(worktree, new_head)
        .ok()
        .flatten()
        .filter(|old_head| is_valid_oid(old_head) && !is_zero_oid(old_head))
}

fn commit_parent_head_for_capture(repo: &Repository, commit_sha: &str) -> Option<String> {
    let commit = repo.find_commit(commit_sha.to_string()).ok()?;
    commit.parent(0).ok().map(|parent| parent.id().to_string())
}

fn stable_carryover_heads_for_command(
    repo: &Repository,
    input: &CarryoverCaptureInput<'_>,
    parsed: &ParsedGitInvocation,
) -> Result<Option<(String, String)>, GitAiError> {
    let command = parsed.command.as_deref().or(input.primary_command);
    let Some(command) = command else {
        return Ok(None);
    };

    let post_head = input
        .post_repo
        .and_then(|repo| repo.head.clone())
        .filter(|head| is_valid_oid(head) && !is_zero_oid(head));
    let ref_head_change = stable_head_change_from_ref_changes(input.ref_changes);
    let rebase_start_target_hint = if command == "rebase" {
        rebase_start_target_hint_from_args(&parsed.command_args)
    } else {
        None
    };

    let resolved = match command {
        "commit" => {
            let new_head = ref_head_change
                .as_ref()
                .map(|(_, new_head)| new_head.clone())
                .or_else(|| post_head.clone())
                .ok_or_else(|| {
                    GitAiError::Generic(format!(
                        "commit missing stable post-head for carryover capture sid={}",
                        input.root_sid
                    ))
                })?;
            let old_head = ref_head_change
                .as_ref()
                .map(|(old_head, _)| old_head.clone())
                .filter(|old_head| !is_zero_oid(old_head))
                .or_else(|| stable_old_head_from_worktree_head_reflog(input.worktree, &new_head))
                .or_else(|| {
                    if parsed.has_command_flag("--amend") {
                        None
                    } else {
                        commit_parent_head_for_capture(repo, &new_head)
                    }
                })
                .unwrap_or_else(|| "initial".to_string());
            Some((old_head, new_head))
        }
        "rebase" | "pull" => ActorDaemonCoordinator::stable_rebase_heads_from_worktree(
            repo,
            input.worktree,
            input.argv,
            rebase_start_target_hint.as_deref(),
        )?
        .map(|(old_head, new_head, _onto_head)| (old_head, new_head))
        .or_else(|| {
            ref_head_change.clone().or_else(|| {
                let new_head = post_head.clone()?;
                let old_head =
                    stable_old_head_from_worktree_head_reflog(input.worktree, &new_head)?;
                Some((old_head, new_head))
            })
        }),
        "checkout" | "switch" => {
            let is_merge = parsed.has_command_flag("--merge") || parsed.has_command_flag("-m");
            if !is_merge {
                None
            } else {
                ref_head_change.clone().or_else(|| {
                    let new_head = post_head.clone()?;
                    let old_head =
                        stable_old_head_from_worktree_head_reflog(input.worktree, &new_head)?;
                    Some((old_head, new_head))
                })
            }
        }
        "reset" => {
            if parsed.has_command_flag("--hard") {
                None
            } else if let Some((old_head, new_head)) = ref_head_change.clone() {
                Some((old_head, new_head))
            } else {
                let new_head = post_head
                    .clone()
                    .or_else(|| stable_new_head_from_ref_changes(input.ref_changes))
                    .ok_or_else(|| {
                        GitAiError::Generic(format!(
                            "reset missing stable head for carryover capture sid={}",
                            input.root_sid
                        ))
                    })?;
                let old_head = stable_old_head_from_worktree_head_reflog(input.worktree, &new_head)
                    .unwrap_or_else(|| new_head.clone());
                Some((old_head, new_head))
            }
        }
        _ => None,
    };

    Ok(resolved)
}

fn resolve_explicit_rebase_branch_ref(worktree: &Path, argv: &[String]) -> Option<String> {
    let parsed = parse_git_cli_args(trace_invocation_args(argv));
    if parsed.command.as_deref() != Some("rebase") {
        return None;
    }

    let branch_spec = explicit_rebase_branch_arg(&parsed.command_args)?;
    let branch_ref = explicit_rebase_branch_ref_name(&branch_spec)?;
    read_ref_oid_for_worktree(worktree, &branch_ref).map(|_| branch_ref)
}

fn explicit_rebase_branch_ref_name(branch_spec: &str) -> Option<String> {
    if branch_spec.starts_with("refs/") {
        return Some(branch_spec.to_string());
    }
    if is_valid_oid(branch_spec) || branch_spec == "HEAD" || branch_spec.starts_with("@{") {
        return None;
    }
    Some(format!("refs/heads/{}", branch_spec))
}

fn resolve_stash_target_oid_for_command(
    worktree: &Path,
    argv: &[String],
) -> Result<Option<String>, GitAiError> {
    let parsed = parse_git_cli_args(trace_invocation_args(argv));
    if parsed.command.as_deref() != Some("stash") {
        return Ok(None);
    }
    if !stash_requires_target_resolution(&parsed.command_args) {
        return Ok(None);
    }

    let target_spec = stash_target_spec(&parsed.command_args);
    let resolved =
        resolve_stash_target_oid_for_worktree(worktree, target_spec).ok_or_else(|| {
            GitAiError::Generic(format!(
                "failed to resolve stash target oid from repo state (spec={:?}, worktree={})",
                target_spec,
                worktree.display()
            ))
        })?;
    Ok(Some(resolved))
}

fn stash_target_spec_is_top_of_stack(target_spec: Option<&str>) -> bool {
    matches!(
        target_spec.unwrap_or("stash@{0}"),
        "stash@{0}" | "refs/stash" | "stash"
    )
}
fn resolve_stash_target_oid_for_terminal_payload(
    worktree: &Path,
    argv: &[String],
    ref_changes: &[crate::daemon::domain::RefChange],
) -> Result<Option<String>, GitAiError> {
    let parsed = parse_git_cli_args(trace_invocation_args(argv));
    if parsed.command.as_deref() != Some("stash") {
        return Ok(None);
    }

    let subcommand = parsed
        .command_args
        .first()
        .map(String::as_str)
        .unwrap_or("push");

    // For push/save: resolve the newly-created stash SHA from ref_changes only.
    // We cannot fall back to reading refs/stash from the filesystem because under
    // heavy load the ingress may process this terminal event after a subsequent
    // push has already overwritten refs/stash with a different OID.
    if matches!(subcommand, "push" | "save" | "") {
        let oid = ref_changes
            .iter()
            .rfind(|change| change.reference == "refs/stash")
            .map(|change| change.new.trim().to_string())
            .filter(|oid| !oid.is_empty() && !is_zero_oid(oid));
        return Ok(oid);
    }

    if !stash_requires_target_resolution(&parsed.command_args) {
        return Ok(None);
    }

    let target_spec = stash_target_spec(&parsed.command_args);
    match subcommand {
        "apply" => resolve_stash_target_oid_for_worktree(worktree, target_spec)
            .ok_or_else(|| {
                GitAiError::Generic(format!(
                    "failed to resolve stash apply target oid from terminal repo state (spec={:?}, worktree={})",
                    target_spec,
                    worktree.display()
                ))
            })
            .map(Some),
        "pop" | "drop" | "branch" => {
            if let Some(target_oid) = ref_changes
                .iter()
                .rfind(|change| change.reference == "refs/stash")
                .map(|change| change.old.trim().to_string())
                .filter(|oid| !oid.is_empty() && !is_zero_oid(oid))
            {
                return Ok(Some(target_oid));
            }
            if stash_target_spec_is_top_of_stack(target_spec) {
                return latest_reflog_old_oid_for_worktree(worktree, "refs/stash")
                    .ok_or_else(|| {
                        GitAiError::Generic(format!(
                            "failed to resolve stash {:?} target oid from terminal reflog state (spec={:?}, worktree={})",
                            parsed.command_args.first().map(String::as_str).unwrap_or("stash"),
                            target_spec,
                            worktree.display()
                        ))
                    })
                    .map(Some);
            }
            Err(GitAiError::Generic(format!(
                "failed to resolve stash {:?} target oid from terminal state for non-top stash reference (spec={:?}, worktree={})",
                parsed.command_args.first().map(String::as_str).unwrap_or("stash"),
                target_spec,
                worktree.display()
            )))
        }
        _ => Ok(None),
    }
}

fn resolve_rebase_original_head_for_worktree(worktree: &Path) -> Option<String> {
    let git_dir = git_dir_for_worktree(worktree)?;

    for candidate in [
        git_dir.join("rebase-merge").join("orig-head"),
        git_dir.join("rebase-apply").join("orig-head"),
        git_dir.join("ORIG_HEAD"),
    ] {
        if let Ok(contents) = fs::read_to_string(candidate)
            && let Some(oid) = contents
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
            && is_valid_oid(oid)
            && !is_zero_oid(oid)
        {
            return Some(oid.to_string());
        }
    }

    read_ref_oid_for_worktree(worktree, "ORIG_HEAD")
        .filter(|oid| is_valid_oid(oid) && !is_zero_oid(oid))
}

fn resolve_rebase_onto_for_worktree(worktree: &Path) -> Option<String> {
    let git_dir = git_dir_for_worktree(worktree)?;
    let onto_path = git_dir.join("rebase-merge").join("onto");
    if let Ok(contents) = fs::read_to_string(onto_path)
        && let Some(oid) = contents.lines().map(str::trim).find(|l| !l.is_empty())
        && is_valid_oid(oid)
        && !is_zero_oid(oid)
    {
        return Some(oid.to_string());
    }
    let onto_path = git_dir.join("rebase-apply").join("onto");
    if let Ok(contents) = fs::read_to_string(onto_path)
        && let Some(oid) = contents.lines().map(str::trim).find(|l| !l.is_empty())
        && is_valid_oid(oid)
        && !is_zero_oid(oid)
    {
        return Some(oid.to_string());
    }
    None
}

type MergeSquashSnapshot = String;

fn capture_merge_squash_source_head_for_command(
    worktree: &Path,
    _primary_command: Option<&str>,
    argv: &[String],
    exit_code: i32,
) -> Result<Option<String>, GitAiError> {
    if exit_code != 0 {
        return Ok(None);
    }

    let parsed = parse_git_cli_args(trace_invocation_args(argv));
    if parsed.command.as_deref() != Some("merge")
        || !parsed.command_args.iter().any(|arg| arg == "--squash")
    {
        return Ok(None);
    }

    let source_head = resolve_squash_source_head_for_worktree(worktree).ok_or_else(|| {
        GitAiError::Generic(format!(
            "merge --squash missing source head from MERGE_HEAD/SQUASH_MSG worktree={}",
            worktree.display()
        ))
    })?;
    Ok(Some(source_head))
}

fn capture_inflight_merge_squash_source_head_for_commit(
    worktree: &Path,
    primary_command: Option<&str>,
    argv: &[String],
) -> Result<Option<String>, GitAiError> {
    if primary_command != Some("commit") {
        return Ok(None);
    }

    let parsed = parse_git_cli_args(trace_invocation_args(argv));
    if parsed.command.as_deref() != Some("commit") && primary_command != Some("commit") {
        return Ok(None);
    }

    let Some(source_head) = resolve_squash_source_head_for_worktree(worktree) else {
        return Ok(None);
    };
    Ok(Some(source_head))
}

fn tracked_reflog_refs_for_command(
    command: Option<&str>,
    repo: Option<&RepoContext>,
    worktree: &Path,
    argv: &[String],
) -> Vec<String> {
    let mut refs = Vec::new();
    if let Some(branch) = repo.and_then(|repo| repo.branch.as_deref()) {
        refs.push(format!("refs/heads/{}", branch));
    }
    if command == Some("rebase")
        && let Some(branch_ref) = resolve_explicit_rebase_branch_ref(worktree, argv)
    {
        refs.push(branch_ref);
    }
    if matches!(
        command,
        Some("reset" | "merge" | "pull" | "rebase" | "cherry-pick" | "checkout" | "switch")
    ) {
        refs.push("ORIG_HEAD".to_string());
    }
    if command == Some("stash") {
        refs.push("refs/stash".to_string());
    }
    refs.sort();
    refs.dedup();
    refs
}

fn daemon_reflog_offsets_for_refs(
    worktree: &Path,
    refs: &[String],
) -> Option<HashMap<String, u64>> {
    let common_dir = common_dir_for_worktree(worktree)?;
    let logs_dir = common_dir.join("logs");
    let mut offsets = HashMap::new();
    for reference in refs {
        let path = logs_dir.join(reference);
        let len = fs::metadata(&path)
            .ok()
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        offsets.insert(reference.clone(), len);
    }
    Some(offsets)
}

fn daemon_parse_reflog_line(
    reference: &str,
    line: &str,
) -> Option<crate::daemon::domain::RefChange> {
    let head = line.split('\t').next().unwrap_or_default();
    let mut parts = head.split_whitespace();
    let old = parts.next()?.trim();
    let new = parts.next()?.trim();
    if !is_valid_oid(old) || !is_valid_oid(new) || old == new {
        return None;
    }
    Some(crate::daemon::domain::RefChange {
        reference: reference.to_string(),
        old: old.to_string(),
        new: new.to_string(),
    })
}

fn daemon_reflog_delta_from_offsets(
    worktree: &Path,
    start_offsets: &HashMap<String, u64>,
    end_offsets: &HashMap<String, u64>,
) -> Result<Vec<crate::daemon::domain::RefChange>, GitAiError> {
    let common_dir = common_dir_for_worktree(worktree).ok_or_else(|| {
        GitAiError::Generic(format!(
            "failed to resolve common dir for worktree {}",
            worktree.display()
        ))
    })?;
    let refs = start_offsets
        .keys()
        .chain(end_offsets.keys())
        .cloned()
        .collect::<std::collections::HashSet<_>>();

    let mut out = Vec::new();
    for reference in refs {
        let start_offset = start_offsets.get(&reference).copied().unwrap_or(0);
        let end_offset = end_offsets.get(&reference).copied().unwrap_or(start_offset);
        if end_offset < start_offset {
            return Err(GitAiError::Generic(format!(
                "reflog cut regressed for {} ({} < {})",
                reference, end_offset, start_offset
            )));
        }
        if end_offset == start_offset {
            continue;
        }

        let path = common_dir.join("logs").join(&reference);
        if !path.exists() {
            return Err(GitAiError::Generic(format!(
                "reflog path missing for {}: {}",
                reference,
                path.display()
            )));
        }
        let metadata = fs::metadata(&path)?;
        if metadata.len() < end_offset {
            return Err(GitAiError::Generic(format!(
                "reflog shorter than cut for {} ({} < {})",
                reference,
                metadata.len(),
                end_offset
            )));
        }

        let mut file = File::open(&path)?;
        file.seek(SeekFrom::Start(start_offset))?;
        let reader = BufReader::new(file.take(end_offset.saturating_sub(start_offset)));
        for line in reader.lines() {
            let line = line?;
            if let Some(change) = daemon_parse_reflog_line(&reference, &line) {
                out.push(change);
            }
        }
    }
    Ok(out)
}

fn apply_checkpoint_side_effect(request: CheckpointRequest) -> Result<(), GitAiError> {
    if request.files.is_empty() {
        return Ok(());
    }

    let repo_work_dir = &request.files[0].repo_work_dir;
    let repo = match discover_repository_in_path_no_git_exec(repo_work_dir) {
        Ok(repo) => repo,
        Err(e) => {
            if request.checkpoint_kind.is_ai()
                && let Some(ref agent_id) = request.agent_id
                && crate::daemon::checkpoint::should_emit_agent_usage(agent_id)
            {
                let attrs = crate::daemon::checkpoint::build_agent_usage_attrs(None, agent_id);
                let values = crate::metrics::AgentUsageValues::new();
                crate::metrics::record(values, attrs);
            }
            return Err(e);
        }
    };
    let author = repo.git_author_identity().formatted_or_unknown();

    if request.checkpoint_kind.is_ai()
        && let Some(ref agent_id) = request.agent_id
        && crate::daemon::checkpoint::should_emit_agent_usage(agent_id)
    {
        let attrs = crate::daemon::checkpoint::build_agent_usage_attrs(Some(&repo), agent_id);
        let values = crate::metrics::AgentUsageValues::new();
        crate::metrics::record(values, attrs);
    }

    let resolved = resolve_checkpoint_request(&repo, &request)?;
    let Some(resolved) = resolved else {
        return Ok(());
    };

    crate::daemon::checkpoint::execute_resolved_checkpoint_from_daemon(
        &repo,
        &author,
        request.checkpoint_kind,
        request,
        resolved,
    )
}

fn resolve_checkpoint_request(
    repo: &crate::git::repository::Repository,
    request: &CheckpointRequest,
) -> Result<Option<crate::daemon::checkpoint::ResolvedCheckpointExecution>, GitAiError> {
    use crate::authorship::ignore::{
        build_ignore_matcher, effective_ignore_patterns, should_ignore_file_with_matcher,
    };
    use crate::commands::checkpoint_agent::orchestrator::BaseCommit;
    use crate::utils::normalize_to_posix;

    let Some(first_file) = request.files.first() else {
        return Ok(None);
    };
    let base_commit = match &first_file.base_commit {
        BaseCommit::Sha(sha) => sha.clone(),
        BaseCommit::Initial => "initial".to_string(),
    };

    let repo_workdir = repo.workdir()?;
    let canonical_workdir = repo_workdir.canonicalize().unwrap_or(repo_workdir.clone());
    let ignore_patterns = effective_ignore_patterns(repo, &[], &[]);
    let ignore_matcher = build_ignore_matcher(&ignore_patterns);

    let mut files = Vec::new();
    let mut dirty_files = HashMap::new();
    let mut seen = std::collections::HashSet::new();

    for file in &request.files {
        let path_str = file.path.to_string_lossy();
        let path_str = path_str.trim();
        if path_str.is_empty() {
            continue;
        }

        let abs_path = if file.path.is_absolute() {
            file.path.clone()
        } else {
            repo_workdir.join(&*file.path)
        };
        if !repo.path_is_in_workdir(&abs_path) {
            continue;
        }

        let relative_path = abs_path
            .canonicalize()
            .unwrap_or(abs_path.clone())
            .strip_prefix(&canonical_workdir)
            .map(|p| normalize_to_posix(&p.to_string_lossy()))
            .unwrap_or_else(|_| {
                abs_path
                    .strip_prefix(&repo_workdir)
                    .map(|p| normalize_to_posix(&p.to_string_lossy()))
                    .unwrap_or_else(|_| normalize_to_posix(path_str))
            });

        if !seen.insert(relative_path.clone()) {
            continue;
        }
        if should_ignore_file_with_matcher(&relative_path, &ignore_matcher) {
            continue;
        }

        if let Some(content) = &file.content
            && !content.chars().any(|c| c == '\0')
        {
            dirty_files.insert(relative_path.clone(), content.clone());
            files.push(relative_path);
        }
    }

    if files.is_empty() {
        return Ok(None);
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    Ok(Some(
        crate::daemon::checkpoint::ResolvedCheckpointExecution {
            base_commit,
            ts,
            files,
            dirty_files,
        },
    ))
}

fn compute_watermarks_from_stat(
    repo_working_dir: &str,
    file_paths: &[String],
) -> std::collections::HashMap<String, u128> {
    let repo_root = std::path::Path::new(repo_working_dir);
    let mut watermarks = std::collections::HashMap::new();
    for path in file_paths {
        let full_path = repo_root.join(path);
        if let Ok(metadata) = std::fs::symlink_metadata(&full_path)
            && let Ok(mtime) = metadata.modified()
        {
            let nanos = mtime
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            // Normalize watermark keys the same way bash_tool::normalize_path does
            // so that case-folded snapshot lookups on macOS/Windows find a match.
            let key = crate::commands::checkpoint_agent::bash_tool::normalize_path(
                std::path::Path::new(path),
            )
            .to_string_lossy()
            .to_string();
            watermarks.insert(key, nanos);
        }
    }
    watermarks
}

fn parsed_invocation_for_side_effect(
    command: Option<&str>,
    args: &[String],
) -> ParsedGitInvocation {
    ParsedGitInvocation {
        global_args: Vec::new(),
        command: command.map(ToString::to_string),
        command_args: args.to_vec(),
        saw_end_of_opts: false,
        is_help: command == Some("help") || args.iter().any(|arg| arg == "-h" || arg == "--help"),
    }
}

fn parsed_invocation_for_normalized_command(
    cmd: &crate::daemon::domain::NormalizedCommand,
) -> ParsedGitInvocation {
    if !cmd.raw_argv.is_empty() {
        return parse_git_cli_args(trace_invocation_args(&cmd.raw_argv));
    }

    if cmd.primary_command.is_some() || !cmd.invoked_args.is_empty() {
        return parsed_invocation_for_side_effect(
            cmd.primary_command.as_deref(),
            &cmd.invoked_args,
        );
    }

    ParsedGitInvocation {
        global_args: Vec::new(),
        command: None,
        command_args: Vec::new(),
        saw_end_of_opts: false,
        is_help: false,
    }
}

fn apply_push_side_effect(
    worktree: &str,
    command: Option<&str>,
    args: &[String],
) -> Result<(), GitAiError> {
    use crate::config::NotesBackendKind;
    use crate::git::cli_parser::is_dry_run;
    use crate::git::sync_authorship::push_authorship_notes;

    if crate::config::Config::get().notes_backend_kind() == NotesBackendKind::Http {
        tracing::debug!("apply_push_side_effect: skipping authorship push (Http backend)");
        return Ok(());
    }

    let repo = find_repository_in_path(worktree)?;
    let parsed = parsed_invocation_for_side_effect(command, args);

    if is_dry_run(&parsed.command_args)
        || parsed
            .command_args
            .iter()
            .any(|a| a == "-d" || a == "--delete")
        || parsed.command_args.iter().any(|a| a == "--mirror")
    {
        return Ok(());
    }

    let remote = resolve_push_remote_for_side_effect(&parsed, &repo);
    let Some(remote) = remote else {
        tracing::debug!("no remotes found for authorship push; skipping");
        return Ok(());
    };

    crate::commands::upgrade::maybe_schedule_background_update_check();
    tracing::debug!("started pushing authorship notes to remote: {}", remote);

    if let Err(e) = push_authorship_notes(&repo, &remote) {
        tracing::debug!("authorship push failed: {}", e);
    }
    Ok(())
}

fn resolve_push_remote_for_side_effect(
    parsed_args: &crate::git::cli_parser::ParsedGitInvocation,
    repository: &Repository,
) -> Option<String> {
    let remotes = repository.remotes().ok();
    let remote_names: Vec<String> = remotes
        .as_ref()
        .map(|r| {
            (0..r.len())
                .filter_map(|i| r.get(i).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let upstream_remote = repository.upstream_remote().ok().flatten();
    let default_remote = repository.get_default_remote().ok().flatten();

    let positional_remote = parsed_args
        .command_args
        .iter()
        .find(|arg| !arg.starts_with('-') && remote_names.contains(arg))
        .cloned();

    positional_remote.or(upstream_remote).or(default_remote)
}

fn apply_pull_notes_sync_side_effect(
    worktree: &str,
    command: Option<&str>,
    args: &[String],
) -> Result<(), GitAiError> {
    let repo = find_repository_in_path(worktree)?;
    let parsed = parsed_invocation_for_side_effect(command, args);
    let remote = match fetch_remote_from_args(&repo, &parsed) {
        Ok(remote) => remote,
        Err(error) => {
            tracing::debug!(
                %error,
                command = parsed.command.as_deref().unwrap_or("pull"),
                "notes sync: failed to determine remote"
            );
            return Ok(());
        }
    };
    if let Err(error) = fetch_authorship_notes(&repo, &remote) {
        tracing::debug!(
            %error,
            %remote,
            "notes sync: failed to fetch authorship notes"
        );
    }
    Ok(())
}

fn apply_clone_notes_sync_side_effect(worktree: &str) -> Result<(), GitAiError> {
    let repo = find_repository_in_path(worktree)?;
    if let Err(error) = fetch_authorship_notes(&repo, "origin") {
        tracing::debug!(
            %error,
            "notes sync: failed to fetch clone authorship notes from origin"
        );
    }
    Ok(())
}

fn apply_pull_fast_forward_working_log_side_effect(
    worktree: &str,
    old_head: &str,
    new_head: &str,
) -> Result<(), GitAiError> {
    let repo = find_repository_in_path(worktree)?;
    repo.storage.rename_working_log(old_head, new_head)?;
    Ok(())
}

fn remove_working_log_attributions_for_pathspecs(
    repository: &Repository,
    head: &str,
    pathspecs: &[String],
) -> Result<(), GitAiError> {
    let working_log = repository.storage.working_log_for_base_commit(head)?;

    let initial = working_log.read_initial_attributions();
    if !initial.files.is_empty() {
        let filtered_files = initial
            .files
            .into_iter()
            .filter(|(file, _)| !matches_any_pathspec(file, pathspecs))
            .collect();
        let mut filtered_blobs = initial.file_blobs;
        filtered_blobs.retain(|file, _| !matches_any_pathspec(file, pathspecs));
        working_log.write_initial(crate::git::repo_storage::InitialAttributions {
            files: filtered_files,
            prompts: initial.prompts,
            file_blobs: filtered_blobs,
            humans: initial.humans,
            sessions: initial.sessions,
        })?;
    }

    let checkpoints = working_log.read_all_checkpoints()?;
    let filtered: Vec<_> = checkpoints
        .into_iter()
        .map(|mut checkpoint| {
            checkpoint
                .entries
                .retain(|entry| !matches_any_pathspec(&entry.file, pathspecs));
            checkpoint
        })
        .filter(|checkpoint| !checkpoint.entries.is_empty())
        .collect();
    working_log.write_all_checkpoints(&filtered)?;
    Ok(())
}

fn apply_checkout_switch_working_log_side_effect(
    cmd: &crate::daemon::domain::NormalizedCommand,
    carryover_snapshot: Option<&HashMap<String, String>>,
) -> Result<(), GitAiError> {
    let Some(worktree) = cmd.worktree.as_ref() else {
        return Ok(());
    };
    let repo = find_repository_in_path(&worktree.to_string_lossy())?;
    let parsed = parsed_invocation_for_normalized_command(cmd);
    let old_head = cmd
        .pre_repo
        .as_ref()
        .and_then(|repo| repo.head.as_deref())
        .unwrap_or_default()
        .to_string();
    let new_head = cmd
        .post_repo
        .as_ref()
        .and_then(|repo| repo.head.as_deref())
        .filter(|h| !h.is_empty())
        .map(|h| h.to_string())
        .or_else(|| read_head_state_for_worktree(worktree).and_then(|s| s.head))
        .unwrap_or_default();

    if cmd.primary_command.as_deref() == Some("checkout") {
        let pathspecs = parsed.pathspecs();
        if !pathspecs.is_empty() {
            if !old_head.is_empty() {
                remove_working_log_attributions_for_pathspecs(&repo, &old_head, &pathspecs)?;
            }
            return Ok(());
        }
    }

    if old_head.is_empty() || new_head.is_empty() || old_head == new_head {
        return Ok(());
    }

    let is_merge = parsed.has_command_flag("--merge") || parsed.has_command_flag("-m");
    let is_force = match cmd.primary_command.as_deref() {
        Some("checkout") => parsed.has_command_flag("--force") || parsed.has_command_flag("-f"),
        Some("switch") => {
            parsed.has_command_flag("--discard-changes")
                || parsed.has_command_flag("--force")
                || parsed.has_command_flag("-f")
        }
        _ => false,
    };

    if is_force {
        repo.storage.delete_working_log_for_base_commit(&old_head)?;
        return Ok(());
    }

    if is_merge {
        let tracked_files = tracked_working_log_files(&repo, &old_head)?;
        if !tracked_files.is_empty() && carryover_snapshot.is_none() {
            // Carryover snapshot was not captured (e.g. the trace arrived before
            // the worktree reflog was fully populated, or the wrapper already
            // handled the migration).  Fall through to the rename path so the
            // working log is migrated rather than lost.  Attribution may be
            // slightly misaligned but is preserved.
            tracing::warn!(
                command = cmd.primary_command.as_deref().unwrap_or("checkout"),
                "--merge missing carryover snapshot, falling back to rename"
            );
        } else {
            if let Some(snapshot) = carryover_snapshot {
                // Fix #957: When --merge produced conflict markers (exit_code != 0),
                // the snapshot files contain conflict markers.  Strip them before
                // restoring working-log carryover so byte-level attributions align
                // with the clean content.
                let clean_snapshot: HashMap<String, String> = if cmd.exit_code != 0 {
                    snapshot
                        .iter()
                        .map(|(k, v)| {
                            let clean = if crate::authorship::virtual_attribution::content_has_conflict_markers(v) {
                                crate::authorship::virtual_attribution::strip_conflict_markers_keep_ours(v)
                            } else {
                                v.clone()
                            };
                            (k.clone(), clean)
                        })
                        .collect()
                } else {
                    snapshot.clone()
                };
                restore_working_log_carryover(
                    &repo,
                    &old_head,
                    &new_head,
                    clean_snapshot,
                    Some(repo.git_author_identity().formatted_or_unknown()),
                )?;
            }
            repo.storage.delete_working_log_for_base_commit(&old_head)?;
            return Ok(());
        }
    }

    repo.storage.rename_working_log(&old_head, &new_head)?;
    Ok(())
}

fn recent_checkout_switch_prerequisite_from_command(
    cmd: &crate::daemon::domain::NormalizedCommand,
    carryover_snapshot: Option<&HashMap<String, String>>,
) -> Option<RecentReplayPrerequisite> {
    let parsed = parsed_invocation_for_normalized_command(cmd);
    let old_head = cmd
        .pre_repo
        .as_ref()
        .and_then(|repo| repo.head.as_deref())
        .unwrap_or_default()
        .to_string();
    let new_head = cmd
        .post_repo
        .as_ref()
        .and_then(|repo| repo.head.as_deref())
        .unwrap_or_default()
        .to_string();

    if old_head.is_empty() || new_head.is_empty() || old_head == new_head {
        return None;
    }

    if cmd.primary_command.as_deref() == Some("checkout") && !parsed.pathspecs().is_empty() {
        return None;
    }

    let is_force = match cmd.primary_command.as_deref() {
        Some("checkout") => parsed.has_command_flag("--force") || parsed.has_command_flag("-f"),
        Some("switch") => {
            parsed.has_command_flag("--discard-changes")
                || parsed.has_command_flag("--force")
                || parsed.has_command_flag("-f")
        }
        _ => false,
    };
    if is_force {
        return None;
    }

    let is_merge = parsed.has_command_flag("--merge") || parsed.has_command_flag("-m");
    if is_merge {
        return carryover_snapshot.and_then(|snapshot| {
            (!snapshot.is_empty()).then(|| {
                // Strip conflict markers before storing so the replay path receives
                // clean content.  Mirrors the stripping done in the direct side-effect
                // path (apply_checkout_switch_working_log_side_effect) for the same
                // reason: --merge with exit_code != 0 leaves conflict markers on disk.
                let clean_state: HashMap<String, String> = if cmd.exit_code != 0 {
                    snapshot
                        .iter()
                        .map(|(k, v)| {
                            let clean = if crate::authorship::virtual_attribution::content_has_conflict_markers(v) {
                                crate::authorship::virtual_attribution::strip_conflict_markers_keep_ours(v)
                            } else {
                                v.clone()
                            };
                            (k.clone(), clean)
                        })
                        .collect()
                } else {
                    snapshot.clone()
                };
                RecentReplayPrerequisite::CheckoutSwitchMerge {
                    target_head: new_head,
                    old_head,
                    final_state: clean_state,
                }
            })
        });
    }

    Some(RecentReplayPrerequisite::CheckoutSwitchRename {
        target_head: new_head,
        old_head,
    })
}
fn family_key_for_repository(repo: &Repository) -> String {
    repo.common_dir()
        .canonicalize()
        .unwrap_or_else(|_| repo.common_dir().to_path_buf())
        .to_string_lossy()
        .to_string()
}
fn is_valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_zero_oid(oid: &str) -> bool {
    is_valid_oid(oid) && oid.chars().all(|c| c == '0')
}

fn is_non_auxiliary_ref(reference: &str) -> bool {
    !(reference.starts_with("refs/notes/")
        || reference.starts_with("refs/tags/")
        || reference.starts_with("refs/replace/"))
}

fn processed_rebase_new_heads(_repository: &Repository) -> Result<HashSet<String>, GitAiError> {
    // The rewrite log has been removed; no previously-processed heads to report.
    Ok(HashSet::new())
}

/// Check whether `ancestor` is an ancestor of `descendant` using
/// `git merge-base --is-ancestor`.
fn is_ancestor_commit(repository: &Repository, ancestor: &str, descendant: &str) -> bool {
    let mut args = repository.global_args_for_exec();
    args.push("merge-base".to_string());
    args.push("--is-ancestor".to_string());
    args.push(ancestor.to_string());
    args.push(descendant.to_string());
    crate::git::repository::exec_git(&args).is_ok()
}

fn repo_is_ancestor(
    repository: &crate::git::repository::Repository,
    ancestor: &str,
    descendant: &str,
) -> bool {
    let mut args = repository.global_args_for_exec();
    args.push("merge-base".to_string());
    args.push("--is-ancestor".to_string());
    args.push(ancestor.to_string());
    args.push(descendant.to_string());
    exec_git(&args).is_ok()
}

/// Collect positional arguments from a cherry-pick command as potential commit
/// references. Unlike the full-OID-only `is_valid_oid` check, this accepts short SHA prefixes and
/// symbolic refs (e.g. branch names) that git would resolve on the command line.
/// Resolution to full OIDs happens later in `resolve_cherry_pick_source_refs`
/// which runs in the async side-effect path.
fn cherry_pick_source_refs_from_command(
    cmd: &crate::daemon::domain::NormalizedCommand,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    for arg in &cmd.invoked_args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--abort" || arg == "--continue" || arg == "--quit" || arg == "--skip" {
            return Vec::new();
        }
        if matches!(
            arg.as_str(),
            "-m" | "--mainline" | "-X" | "--strategy-option" | "--strategy"
        ) || arg == "--gpg-sign"
        {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        if !arg.is_empty() && !out.iter().any(|seen: &String| seen == arg) {
            out.push(arg.to_string());
        }
    }
    out
}

fn rebase_is_control_mode(cmd: &crate::daemon::domain::NormalizedCommand) -> bool {
    summarize_rebase_args(&cmd.invoked_args).is_control_mode
}

fn rebase_start_target_hint_from_args(args: &[String]) -> Option<String> {
    let summary = summarize_rebase_args(args);
    if summary.is_control_mode {
        return None;
    }
    if let Some(onto_spec) = summary.onto_spec {
        return Some(onto_spec);
    }
    if summary.has_root {
        return None;
    }
    summary.positionals.first().cloned()
}
fn strict_rebase_original_head_from_command(
    cmd: &crate::daemon::domain::NormalizedCommand,
    semantic_old_head: &str,
) -> Option<String> {
    if let Some(worktree) = cmd.worktree.as_ref()
        && let Some(old_head) = resolve_rebase_original_head_for_worktree(worktree)
    {
        return Some(old_head);
    }

    if is_valid_oid(semantic_old_head) && !is_zero_oid(semantic_old_head) {
        return Some(semantic_old_head.to_string());
    }

    if !rebase_is_control_mode(cmd)
        && let Some(old_head) = cmd
            .pre_repo
            .as_ref()
            .and_then(|repo| repo.head.clone())
            .filter(|head| is_valid_oid(head) && !is_zero_oid(head))
    {
        return Some(old_head);
    }

    if let Some(branch_spec) = explicit_rebase_branch_arg(&cmd.invoked_args)
        && let Some(branch_ref) = explicit_rebase_branch_ref_name(&branch_spec)
        && let Some(old_head) = cmd
            .ref_changes
            .iter()
            .find(|change| {
                change.reference == branch_ref
                    && is_valid_oid(&change.old)
                    && !is_zero_oid(&change.old)
            })
            .map(|change| change.old.clone())
    {
        return Some(old_head);
    }

    if let Some(old_head) = cmd
        .ref_changes
        .iter()
        .find(|change| {
            change.reference.starts_with("refs/heads/")
                && is_valid_oid(&change.old)
                && !is_zero_oid(&change.old)
        })
        .map(|change| change.old.clone())
    {
        return Some(old_head);
    }

    if let Some(old_head) = cmd
        .ref_changes
        .iter()
        .find(|change| {
            change.reference == "HEAD" && is_valid_oid(&change.old) && !is_zero_oid(&change.old)
        })
        .map(|change| change.old.clone())
    {
        return Some(old_head);
    }

    cmd.ref_changes
        .iter()
        .find(|change| {
            change.reference == "ORIG_HEAD"
                && is_valid_oid(&change.new)
                && !is_zero_oid(&change.new)
        })
        .map(|change| change.new.clone())
}
fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn remove_socket_if_exists(path: &Path) -> Result<(), GitAiError> {
    #[cfg(unix)]
    if path.exists() {
        fs::remove_file(path)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(not(windows))]
fn set_socket_owner_only(path: &Path) -> Result<(), GitAiError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn pid_metadata_path(config: &DaemonConfig) -> PathBuf {
    config
        .lock_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(PID_META_FILE)
}

/// Returns the log file path for the currently running daemon, if any.
/// Reads the PID from daemon.pid.json and constructs the log path.
pub fn daemon_log_file_path(config: &DaemonConfig) -> Result<PathBuf, GitAiError> {
    let meta_path = pid_metadata_path(config);
    let contents = fs::read_to_string(&meta_path).map_err(|e| {
        GitAiError::Generic(format!(
            "failed to read daemon pid metadata at {}: {}",
            meta_path.display(),
            e
        ))
    })?;
    let meta: DaemonPidMeta = serde_json::from_str(&contents)?;
    let log_dir = config.internal_dir.join("daemon").join("logs");
    Ok(log_dir.join(format!("{}.log", meta.pid)))
}

fn write_pid_metadata(config: &DaemonConfig) -> Result<(), GitAiError> {
    let meta = DaemonPidMeta {
        pid: std::process::id(),
        started_at_ns: now_unix_nanos(),
    };
    let path = pid_metadata_path(config);
    fs::write(path, serde_json::to_string_pretty(&meta)?)?;
    Ok(())
}

/// Read the PID of the currently running daemon from the pid metadata file.
pub fn read_daemon_pid(config: &DaemonConfig) -> Result<u32, GitAiError> {
    let meta_path = pid_metadata_path(config);
    let contents = fs::read_to_string(&meta_path).map_err(|e| {
        GitAiError::Generic(format!(
            "failed to read daemon pid metadata at {}: {}",
            meta_path.display(),
            e
        ))
    })?;
    let meta: DaemonPidMeta = serde_json::from_str(&contents)?;
    Ok(meta.pid)
}

fn remove_pid_metadata(config: &DaemonConfig) -> Result<(), GitAiError> {
    let path = pid_metadata_path(config);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Remove daemon artifacts that may be inaccessible due to ownership mismatch
/// (e.g. left by a prior root invocation). Called only from `run_daemon()` at
/// startup — never from probe functions — so it cannot break flock visibility
/// for read-only lock checks.
#[cfg(unix)]
pub(crate) fn remove_stale_daemon_files(config: &DaemonConfig) {
    let pid_path = pid_metadata_path(config);
    for path in [
        config.lock_path.as_path(),
        config.control_socket_path.as_path(),
        config.trace_socket_path.as_path(),
        pid_path.as_path(),
    ] {
        let dominated_by_wrong_owner = match std::fs::metadata(path) {
            Ok(meta) => {
                use std::os::unix::fs::MetadataExt;
                meta.uid() != unsafe { libc::getuid() }
            }
            Err(_) => false,
        };
        if dominated_by_wrong_owner {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn remove_stale_daemon_files(_config: &DaemonConfig) {}

#[cfg(not(windows))]
fn daemon_is_test_mode() -> bool {
    std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
        || std::env::var_os("GITAI_TEST_DB_PATH").is_some()
}

fn daemon_log_dir(config: &DaemonConfig) -> PathBuf {
    config.internal_dir.join("daemon").join("logs")
}

/// Redirect stdout and stderr to a per-PID log file inside the daemon logs
/// directory. Skipped in test mode to keep test output on the console.
/// Returns a guard that keeps the log file open for the lifetime of the daemon.
#[cfg(unix)]
fn maybe_setup_daemon_log_file(config: &DaemonConfig) -> Option<DaemonLogGuard> {
    if daemon_is_test_mode() {
        return None;
    }
    match setup_daemon_log_file(config) {
        Ok(guard) => Some(guard),
        Err(e) => {
            tracing::error!(%e, "log file setup failed");
            None
        }
    }
}

#[cfg(windows)]
fn maybe_setup_daemon_log_file(config: &DaemonConfig) -> Option<DaemonLogGuard> {
    match setup_daemon_log_file(config) {
        Ok(guard) => Some(guard),
        Err(e) => {
            tracing::error!(%e, "log file setup failed");
            None
        }
    }
}

struct DaemonLogGuard {
    _file: File,
}

#[cfg(unix)]
fn setup_daemon_log_file(config: &DaemonConfig) -> Result<DaemonLogGuard, GitAiError> {
    use std::os::unix::io::AsRawFd;

    let log_dir = daemon_log_dir(config);
    fs::create_dir_all(&log_dir)?;

    let prune_dir = log_dir.clone();
    std::thread::spawn(move || prune_stale_daemon_logs(&prune_dir));

    let log_path = log_dir.join(format!("{}.log", std::process::id()));
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let fd = file.as_raw_fd();
    // SAFETY: dup2 is a standard POSIX call; we redirect stdout/stderr to our
    // open log file descriptor. The file is kept alive by the returned guard.
    unsafe {
        if libc::dup2(fd, libc::STDOUT_FILENO) == -1 {
            return Err(GitAiError::Generic("dup2 stdout failed".to_string()));
        }
        if libc::dup2(fd, libc::STDERR_FILENO) == -1 {
            return Err(GitAiError::Generic("dup2 stderr failed".to_string()));
        }
    }

    Ok(DaemonLogGuard { _file: file })
}

#[cfg(windows)]
fn setup_daemon_log_file(config: &DaemonConfig) -> Result<DaemonLogGuard, GitAiError> {
    let log_dir = daemon_log_dir(config);
    fs::create_dir_all(&log_dir)?;

    let prune_dir = log_dir.clone();
    std::thread::spawn(move || prune_stale_daemon_logs(&prune_dir));

    let log_path = log_dir.join(format!("{}.log", std::process::id()));
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    redirect_windows_stdio_to_log_file(&file)?;
    eprintln!("[git-ai] daemon log initialized at {}", log_path.display());

    Ok(DaemonLogGuard { _file: file })
}

#[cfg(windows)]
fn redirect_windows_stdio_to_log_file(file: &File) -> Result<(), GitAiError> {
    redirect_windows_stdio_stream(file, 1, WINDOWS_STDOUT_HANDLE)?;
    redirect_windows_stdio_stream(file, 2, WINDOWS_STDERR_HANDLE)?;
    Ok(())
}

#[cfg(windows)]
fn redirect_windows_stdio_stream(
    file: &File,
    std_fd: libc::c_int,
    std_handle: u32,
) -> Result<(), GitAiError> {
    let clone = file.try_clone()?;
    let raw_handle = clone.into_raw_handle();
    let fd = unsafe {
        libc::open_osfhandle(
            raw_handle as libc::intptr_t,
            libc::O_APPEND | libc::O_BINARY,
        )
    };
    if fd == -1 {
        unsafe {
            drop(File::from_raw_handle(raw_handle));
        }
        return Err(GitAiError::Generic(format!(
            "open_osfhandle failed for daemon log stream {}: {}",
            std_fd,
            std::io::Error::last_os_error()
        )));
    }

    let dup_result = unsafe { libc::dup2(fd, std_fd) };
    if dup_result == -1 {
        let err = std::io::Error::last_os_error();
        let _ = unsafe { libc::close(fd) };
        return Err(GitAiError::Generic(format!(
            "dup2 failed for daemon log stream {}: {}",
            std_fd, err
        )));
    }
    if unsafe { libc::close(fd) } == -1 {
        tracing::debug!(
            std_fd,
            error = %std::io::Error::last_os_error(),
            "close failed for log stream after successful redirect"
        );
    }

    let set_handle_result = unsafe { SetStdHandle(std_handle, file.as_raw_handle()) };
    if set_handle_result == 0 {
        return Err(GitAiError::Generic(format!(
            "SetStdHandle failed for daemon log stream {}: {}",
            std_fd,
            std::io::Error::last_os_error()
        )));
    }

    Ok(())
}

/// Remove log files from previous daemon runs that are older than one week and
/// whose PID is no longer alive, to avoid unbounded growth while keeping recent
/// logs available for debugging.
fn prune_stale_daemon_logs(log_dir: &Path) {
    let one_week = std::time::Duration::from_secs(7 * 24 * 60 * 60);
    let entries = match fs::read_dir(log_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let _pid: u32 = match stem.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let dominated = path
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > one_week);
        if !dominated {
            continue;
        }
        #[cfg(unix)]
        {
            if process_alive(_pid) {
                continue;
            }
        }
        let _ = fs::remove_file(&path);
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // kill(pid, 0) checks existence without sending a signal.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn read_json_line<R: BufRead>(reader: &mut R) -> Result<Option<String>, GitAiError> {
    let mut line = String::new();
    let read = reader.read_line(&mut line)?;
    if read == 0 {
        return Ok(None);
    }
    Ok(Some(line))
}

#[derive(Debug)]
enum FamilySequencerEntry {
    PendingRoot,
    ReadyCommand(Box<crate::daemon::domain::NormalizedCommand>),
    Checkpoint {
        request: Box<CheckpointRequest>,
        respond_to: Option<oneshot::Sender<Result<u64, GitAiError>>>,
    },
    Canceled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FamilySequencerOrder {
    started_at_ns: u128,
    ordinal: u64,
}

#[derive(Debug, Default)]
struct FamilySequencerState {
    next_ordinal: u64,
    entries: BTreeMap<FamilySequencerOrder, FamilySequencerEntry>,
}

#[derive(Debug, Clone)]
struct PendingRootSlot {
    family: String,
    order: FamilySequencerOrder,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
enum RecentReplayPrerequisite {
    CheckoutSwitchRename {
        target_head: String,
        old_head: String,
    },
    CheckoutSwitchMerge {
        target_head: String,
        old_head: String,
        final_state: HashMap<String, String>,
    },
}

#[derive(Debug, Default, Clone)]
struct TraceIngressState {
    root_worktrees: HashMap<String, PathBuf>,
    root_families: HashMap<String, String>,
    root_argv: HashMap<String, Vec<String>>,
    root_pre_repo: HashMap<String, RepoContext>,
    root_inflight_merge_squash_contexts: HashMap<String, MergeSquashSnapshot>,
    root_terminal_merge_squash_contexts: HashMap<String, MergeSquashSnapshot>,
    root_mutating: HashMap<String, bool>,
    root_target_repo_only: HashMap<String, bool>,
    root_reflog_refs: HashMap<String, Vec<String>>,
    root_head_reflog_start_offsets: HashMap<String, u64>,
    root_family_reflog_start_offsets: HashMap<String, HashMap<String, u64>>,
    root_last_activity_ns: HashMap<String, u64>,
    /// Roots whose start event was identified as definitely read-only. All
    /// subsequent events for these roots (including exit) take the fast path.
    root_definitely_read_only: HashSet<String>,
    root_open_connections: HashMap<String, usize>,
    root_close_fallback_enqueued: HashSet<String>,
}

struct CarryoverCaptureInput<'a> {
    root_sid: &'a str,
    worktree: &'a Path,
    primary_command: Option<&'a str>,
    argv: &'a [String],
    exit_code: i32,
    finished_at_ns: u128,
    post_repo: Option<&'a RepoContext>,
    ref_changes: &'a [crate::daemon::domain::RefChange],
}

#[doc(hidden)]
pub struct ActorDaemonCoordinator {
    backend: Arc<crate::daemon::git_backend::SystemGitBackend>,
    coordinator:
        Arc<crate::daemon::coordinator::Coordinator<crate::daemon::git_backend::SystemGitBackend>>,
    normalizer: AsyncMutex<
        crate::daemon::trace_normalizer::TraceNormalizer<
            crate::daemon::git_backend::SystemGitBackend,
        >,
    >,
    pending_rebase_original_head_by_worktree: Mutex<HashMap<String, (String, Option<String>)>>,
    pending_cherry_pick_sources_by_worktree: Mutex<HashMap<String, Vec<String>>>,
    inflight_effects_by_family: Mutex<HashMap<String, usize>>,
    /// Files with an in-flight AI edit (PreFileEdit received, PostFileEdit not yet completed).
    /// Outer key: family. Inner key: absolute file path string. Value: registration timestamp (nanos).
    pending_ai_edits_by_family: Mutex<HashMap<String, HashMap<String, u128>>>,
    family_sequencers_by_family: Mutex<HashMap<String, FamilySequencerState>>,
    pending_root_slots_by_root: Mutex<HashMap<String, PendingRootSlot>>,
    recent_replay_prerequisites_by_family:
        Mutex<HashMap<String, VecDeque<RecentReplayPrerequisite>>>,
    side_effect_errors_by_family: Mutex<HashMap<String, BTreeMap<u64, String>>>,
    side_effect_exec_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    carryover_snapshots_by_id: Mutex<HashMap<String, HashMap<String, String>>>,
    carryover_snapshot_ids_by_root: Mutex<HashMap<String, Vec<String>>>,
    bash_sessions: Mutex<crate::daemon::bash_sessions::BashSessionState>,
    test_completion_log_dir: Option<PathBuf>,
    test_completion_log_lock: Mutex<()>,
    // OnceLock: set once at worker start, never cleared. The ingest worker
    // exits via the shutdown select! arm instead of relying on channel closure.
    trace_ingest_tx: std::sync::OnceLock<mpsc::Sender<Value>>,
    telemetry_worker: Option<crate::daemon::telemetry_worker::DaemonTelemetryWorkerHandle>,
    transcript_worker: Option<crate::daemon::transcript_worker::TranscriptWorkerHandle>,
    transcript_shutdown_notify: std::sync::OnceLock<Arc<tokio::sync::Notify>>,
    transcripts_db: Option<Arc<crate::transcripts::db::TranscriptsDatabase>>,
    next_trace_ingest_seq: AtomicUsize,
    next_carryover_snapshot_id: AtomicUsize,
    queued_trace_payloads: AtomicUsize,
    queued_trace_payloads_by_root: Mutex<HashMap<String, usize>>,
    processed_trace_ingest_seq: AtomicUsize,
    trace_ingest_progress_notify: Notify,
    trace_ingress_state: Mutex<TraceIngressState>,
    wrapper_states: Mutex<HashMap<String, WrapperStateEntry>>,
    wrapper_state_notify: Notify,
    shutting_down: AtomicBool,
    shutdown_action: AtomicU8,
    shutdown_notify: Notify,
    shutdown_condvar: std::sync::Condvar,
    shutdown_condvar_mutex: Mutex<()>,
}

struct WrapperStateEntry {
    pre_repo: Option<RepoContext>,
    post_repo: Option<RepoContext>,
    received_at_ns: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DaemonExitAction {
    Stop,
    Restart,
    RestartAfterUpdate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DaemonSelfUpdateOutcome {
    Installed,
    NoUpdate,
    Failed,
}

impl DaemonExitAction {
    fn as_u8(self) -> u8 {
        match self {
            Self::Stop => 0,
            Self::Restart => 1,
            Self::RestartAfterUpdate => 2,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Restart,
            2 => Self::RestartAfterUpdate,
            _ => Self::Stop,
        }
    }
}

enum TracePayloadApplyOutcome {
    None,
    Applied(Box<crate::daemon::domain::AppliedCommand>),
    QueuedFamily,
}

impl ActorDaemonCoordinator {
    fn new() -> Self {
        let backend = Arc::new(crate::daemon::git_backend::SystemGitBackend::new());
        Self {
            coordinator: Arc::new(crate::daemon::coordinator::Coordinator::new(
                backend.clone(),
            )),
            normalizer: AsyncMutex::new(crate::daemon::trace_normalizer::TraceNormalizer::new(
                backend.clone(),
            )),
            backend,
            pending_rebase_original_head_by_worktree: Mutex::new(HashMap::new()),
            pending_cherry_pick_sources_by_worktree: Mutex::new(HashMap::new()),
            inflight_effects_by_family: Mutex::new(HashMap::new()),
            pending_ai_edits_by_family: Mutex::new(HashMap::new()),
            family_sequencers_by_family: Mutex::new(HashMap::new()),
            pending_root_slots_by_root: Mutex::new(HashMap::new()),
            recent_replay_prerequisites_by_family: Mutex::new(HashMap::new()),
            side_effect_errors_by_family: Mutex::new(HashMap::new()),
            side_effect_exec_locks: Mutex::new(HashMap::new()),
            carryover_snapshots_by_id: Mutex::new(HashMap::new()),
            carryover_snapshot_ids_by_root: Mutex::new(HashMap::new()),
            bash_sessions: Mutex::new(crate::daemon::bash_sessions::BashSessionState::new()),
            test_completion_log_dir: std::env::var("GIT_AI_TEST_DB_PATH")
                .ok()
                .or_else(|| std::env::var("GITAI_TEST_DB_PATH").ok())
                .map(|_| {
                    DaemonConfig::from_env_or_default_paths()
                        .map(|config| config.test_completion_log_dir())
                        .unwrap_or_else(|_| {
                            std::env::temp_dir().join("git-ai-daemon-test-completions-fallback")
                        })
                }),
            test_completion_log_lock: Mutex::new(()),
            trace_ingest_tx: std::sync::OnceLock::new(),
            telemetry_worker: None,
            transcript_worker: None,
            transcript_shutdown_notify: std::sync::OnceLock::new(),
            transcripts_db: None,
            next_trace_ingest_seq: AtomicUsize::new(0),
            next_carryover_snapshot_id: AtomicUsize::new(0),
            queued_trace_payloads: AtomicUsize::new(0),
            queued_trace_payloads_by_root: Mutex::new(HashMap::new()),
            processed_trace_ingest_seq: AtomicUsize::new(0),
            trace_ingest_progress_notify: Notify::new(),
            trace_ingress_state: Mutex::new(TraceIngressState::default()),
            wrapper_states: Mutex::new(HashMap::new()),
            wrapper_state_notify: Notify::new(),
            shutting_down: AtomicBool::new(false),
            shutdown_action: AtomicU8::new(DaemonExitAction::Stop.as_u8()),
            shutdown_notify: Notify::new(),
            shutdown_condvar: std::sync::Condvar::new(),
            shutdown_condvar_mutex: Mutex::new(()),
        }
    }

    fn is_shutting_down(&self) -> bool {
        // Acquire pairs with the Release store in request_shutdown so all
        // writes made before shutdown is requested are visible to the caller.
        self.shutting_down.load(Ordering::Acquire)
    }

    fn request_shutdown(&self) {
        // Release ensures that any writes made before this store are visible to
        // threads that subsequently load with Acquire (is_shutting_down).
        self.shutting_down.store(true, Ordering::Release);
        // The ingest worker exits via its select! shutdown arm (watching
        // shutdown_notify); we no longer rely on channel closure to stop it.
        self.shutdown_notify.notify_waiters();
        if let Some(transcript_shutdown) = self.transcript_shutdown_notify.get() {
            transcript_shutdown.notify_one();
        }
        // Hold the condvar mutex so notify_all cannot race with the
        // check-then-wait sequence in daemon_update_check_loop.
        let _guard = self
            .shutdown_condvar_mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.shutdown_condvar.notify_all();
    }

    fn request_stop(&self) {
        self.shutdown_action
            .store(DaemonExitAction::Stop.as_u8(), Ordering::SeqCst);
        self.request_shutdown();
    }

    fn request_restart(&self) {
        self.shutdown_action
            .store(DaemonExitAction::Restart.as_u8(), Ordering::SeqCst);
        self.request_shutdown();
    }

    fn request_restart_after_update(&self) {
        self.shutdown_action.store(
            DaemonExitAction::RestartAfterUpdate.as_u8(),
            Ordering::SeqCst,
        );
        self.request_shutdown();
    }

    fn shutdown_action(&self) -> DaemonExitAction {
        DaemonExitAction::from_u8(self.shutdown_action.load(Ordering::SeqCst))
    }

    async fn wait_for_shutdown(&self) {
        // Register the Notified future BEFORE checking the flag so that a
        // request_shutdown() racing between the check and the await cannot
        // slip through without waking us (notify_waiters only wakes futures
        // that are already registered).
        let notified = self.shutdown_notify.notified();
        if self.is_shutting_down() {
            return;
        }
        notified.await;
    }

    fn begin_family_effect(&self, family: &str) -> Result<(), GitAiError> {
        let mut map = self
            .inflight_effects_by_family
            .lock()
            .map_err(|_| GitAiError::Generic("inflight effects map lock poisoned".to_string()))?;
        let entry = map.entry(family.to_string()).or_insert(0);
        *entry = entry.saturating_add(1);
        Ok(())
    }

    fn end_family_effect(&self, family: &str) -> Result<(), GitAiError> {
        let mut map = self
            .inflight_effects_by_family
            .lock()
            .map_err(|_| GitAiError::Generic("inflight effects map lock poisoned".to_string()))?;
        if let Some(entry) = map.get_mut(family) {
            if *entry <= 1 {
                map.remove(family);
            } else {
                *entry -= 1;
            }
        }
        Ok(())
    }

    /// Garbage-collect empty or idle entries from per-family and per-root maps
    /// to prevent unbounded memory growth in long-running daemon processes.
    fn gc_stale_family_state(&self) {
        // NOTE: Do NOT call normalizer.sweep_orphans() here — it removes ALL
        // pending/deferred roots unconditionally which destroys in-flight trace
        // state.  sweep_orphans() is only safe at daemon shutdown.
        if let Ok(mut map) = self.recent_replay_prerequisites_by_family.lock() {
            map.retain(|_, entries| !entries.is_empty());
        }
        if let Ok(mut map) = self.side_effect_errors_by_family.lock() {
            map.retain(|_, errors| !errors.is_empty());
        }
        if let Ok(mut map) = self.family_sequencers_by_family.lock() {
            map.retain(|_, state| !state.entries.is_empty());
        }
        if let Ok(mut map) = self.side_effect_exec_locks.lock() {
            map.retain(|_, lock| Arc::strong_count(lock) <= 1);
        }
        if let Ok(mut map) = self.carryover_snapshots_by_id.lock() {
            map.retain(|_, snapshot| !snapshot.is_empty());
        }
        if let Ok(mut map) = self.carryover_snapshot_ids_by_root.lock() {
            map.retain(|_, ids| !ids.is_empty());
        }
        if let Ok(mut map) = self.pending_rebase_original_head_by_worktree.lock() {
            map.shrink_to_fit();
        }
        if let Ok(mut map) = self.pending_cherry_pick_sources_by_worktree.lock() {
            map.retain(|_, sources| !sources.is_empty());
        }
        if let Ok(mut map) = self.queued_trace_payloads_by_root.lock() {
            map.retain(|_, count| *count > 0);
        }
        // Clean expired pending AI edit entries (older than 10s).
        {
            const PENDING_AI_EDIT_TIMEOUT_NS: u128 = 10_000_000_000;
            let gc_now_ns = now_unix_nanos();
            if let Ok(mut map) = self.pending_ai_edits_by_family.lock() {
                for family_map in map.values_mut() {
                    family_map.retain(|_, registered_at| {
                        gc_now_ns.saturating_sub(*registered_at) < PENDING_AI_EDIT_TIMEOUT_NS
                    });
                }
                map.retain(|_, family_map| !family_map.is_empty());
            }
        }
        // Clean wrapper_states entries older than 60s — these represent wrapper
        // pre/post states that were never consumed by a matching trace2 event.
        let stale_threshold_ns = 60_000_000_000u128; // 60 seconds in nanoseconds
        let now_ns = now_unix_nanos();
        if let Ok(mut map) = self.wrapper_states.lock() {
            map.retain(|_, entry| now_ns.saturating_sub(entry.received_at_ns) < stale_threshold_ns);
        }
    }

    fn canonicalize_path(path: &str) -> String {
        std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string())
    }

    fn register_pending_ai_edits(&self, family: &str, file_paths: &[String]) {
        let now_ns = now_unix_nanos();
        if let Ok(mut map) = self.pending_ai_edits_by_family.lock() {
            let family_map = map.entry(family.to_string()).or_default();
            for file in file_paths {
                family_map.insert(Self::canonicalize_path(file), now_ns);
            }
        }
    }

    fn clear_pending_ai_edits(&self, family: &str, file_paths: &[String]) {
        if let Ok(mut map) = self.pending_ai_edits_by_family.lock()
            && let Some(family_map) = map.get_mut(family)
        {
            for file in file_paths {
                family_map.remove(&Self::canonicalize_path(file));
            }
            if family_map.is_empty() {
                map.remove(family);
            }
        }
    }

    fn file_has_pending_ai_edit(&self, family: &str, file_path: &str) -> bool {
        const PENDING_AI_EDIT_TIMEOUT_NS: u128 = 10_000_000_000; // 10 seconds
        let now_ns = now_unix_nanos();
        let canonical = Self::canonicalize_path(file_path);
        if let Ok(map) = self.pending_ai_edits_by_family.lock()
            && let Some(family_map) = map.get(family)
        {
            return family_map.get(&canonical).is_some_and(|registered_at| {
                now_ns.saturating_sub(*registered_at) < PENDING_AI_EDIT_TIMEOUT_NS
            });
        }
        false
    }

    fn trace_command_participates_in_family_sequencer(primary_command: Option<&str>) -> bool {
        matches!(
            primary_command,
            Some(
                "branch"
                    | "checkout"
                    | "cherry-pick"
                    | "commit"
                    | "fetch"
                    | "merge"
                    | "pull"
                    | "push"
                    | "rebase"
                    | "remote"
                    | "reset"
                    | "revert"
                    | "stash"
                    | "switch"
                    | "tag"
                    | "update-ref"
                    | "worktree"
            )
        )
    }

    fn append_pending_root_entry(
        &self,
        family: &str,
        root_sid: &str,
        started_at_ns: u128,
    ) -> Result<(), GitAiError> {
        {
            let pending_slots = self.pending_root_slots_by_root.lock().map_err(|_| {
                GitAiError::Generic("pending root slots map lock poisoned".to_string())
            })?;
            if pending_slots.contains_key(root_sid) {
                return Ok(());
            }
        }

        let order = {
            let mut sequencers = self.family_sequencers_by_family.lock().map_err(|_| {
                GitAiError::Generic("family sequencer map lock poisoned".to_string())
            })?;
            let state =
                sequencers
                    .entry(family.to_string())
                    .or_insert_with(|| FamilySequencerState {
                        next_ordinal: 1,
                        entries: BTreeMap::new(),
                    });
            let order = FamilySequencerOrder {
                started_at_ns,
                ordinal: state.next_ordinal,
            };
            state.next_ordinal = state.next_ordinal.saturating_add(1);
            state
                .entries
                .insert(order, FamilySequencerEntry::PendingRoot);
            order
        };

        self.pending_root_slots_by_root
            .lock()
            .map_err(|_| GitAiError::Generic("pending root slots map lock poisoned".to_string()))?
            .insert(
                root_sid.to_string(),
                PendingRootSlot {
                    family: family.to_string(),
                    order,
                },
            );
        Ok(())
    }

    fn take_pending_root_slot(
        &self,
        root_sid: &str,
    ) -> Result<Option<PendingRootSlot>, GitAiError> {
        self.pending_root_slots_by_root
            .lock()
            .map_err(|_| GitAiError::Generic("pending root slots map lock poisoned".to_string()))
            .map(|mut slots| slots.remove(root_sid))
    }

    fn maybe_append_pending_root_from_trace_payload(
        &self,
        payload: &Value,
    ) -> Result<(), GitAiError> {
        let event = payload
            .get("event")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if event != "start" {
            return Ok(());
        }

        let Some(sid) = payload.get("sid").and_then(Value::as_str) else {
            return Ok(());
        };
        let root_sid = trace_root_sid(sid);
        if root_sid != sid {
            return Ok(());
        }

        let argv = trace_payload_argv(payload);
        let primary_command =
            trace_payload_primary_command(payload).or_else(|| trace_argv_primary_command(&argv));
        if !Self::trace_command_participates_in_family_sequencer(primary_command.as_deref()) {
            return Ok(());
        }

        let Some(worktree) = trace_payload_worktree_hint(payload) else {
            return Ok(());
        };
        let Some(common_dir) = common_dir_for_worktree(&worktree) else {
            return Ok(());
        };
        let started_at_ns = trace_payload_time_ns(payload).unwrap_or_else(now_unix_nanos);
        let family = common_dir
            .canonicalize()
            .unwrap_or(common_dir)
            .to_string_lossy()
            .to_string();
        self.append_pending_root_entry(&family, root_sid, started_at_ns)
    }

    async fn replace_pending_root_entry(
        &self,
        root_sid: &str,
        replacement: FamilySequencerEntry,
    ) -> Result<Option<String>, GitAiError> {
        let Some(slot) = self.take_pending_root_slot(root_sid)? else {
            return Ok(None);
        };
        let family = slot.family.clone();
        let exec_lock = self.side_effect_exec_lock(&family)?;
        let _guard = exec_lock.lock().await;
        {
            let mut sequencers = self.family_sequencers_by_family.lock().map_err(|_| {
                GitAiError::Generic("family sequencer map lock poisoned".to_string())
            })?;
            let state = sequencers
                .entry(family.clone())
                .or_insert_with(|| FamilySequencerState {
                    next_ordinal: 1,
                    entries: BTreeMap::new(),
                });
            let Some(entry) = state.entries.get_mut(&slot.order) else {
                return Err(GitAiError::Generic(format!(
                    "missing pending root sequencer entry for sid={} family={} order={:?}",
                    root_sid, family, slot.order
                )));
            };
            match entry {
                FamilySequencerEntry::PendingRoot => {
                    *entry = replacement;
                }
                _ => {
                    return Err(GitAiError::Generic(format!(
                        "sequencer entry for sid={} family={} order={:?} was not pending",
                        root_sid, family, slot.order
                    )));
                }
            }
        }
        self.drain_ready_family_sequencer_entries_locked(&family)
            .await?;
        Ok(Some(family))
    }

    fn record_side_effect_error(
        &self,
        family: &str,
        seq: u64,
        error: &GitAiError,
    ) -> Result<(), GitAiError> {
        let mut map = self
            .side_effect_errors_by_family
            .lock()
            .map_err(|_| GitAiError::Generic("side effect errors map lock poisoned".to_string()))?;
        let family_errors = map.entry(family.to_string()).or_insert_with(BTreeMap::new);
        family_errors.insert(seq, error.to_string());
        while family_errors.len() > 256 {
            if let Some(oldest) = family_errors.keys().next().copied() {
                family_errors.remove(&oldest);
            } else {
                break;
            }
        }
        Ok(())
    }

    fn latest_side_effect_error(&self, family: &str) -> Result<Option<String>, GitAiError> {
        let map = self
            .side_effect_errors_by_family
            .lock()
            .map_err(|_| GitAiError::Generic("side effect errors map lock poisoned".to_string()))?;
        Ok(map
            .get(family)
            .and_then(|errors| errors.iter().next_back().map(|(_, error)| error.clone())))
    }

    fn record_recent_replay_prerequisite(
        &self,
        family: &str,
        prerequisite: RecentReplayPrerequisite,
    ) -> Result<(), GitAiError> {
        const MAX_RECENT_REPLAY_PREREQUISITES_PER_FAMILY: usize = 256;

        let mut map = self
            .recent_replay_prerequisites_by_family
            .lock()
            .map_err(|_| {
                GitAiError::Generic("recent replay prerequisites map lock poisoned".to_string())
            })?;
        let entries = map.entry(family.to_string()).or_insert_with(VecDeque::new);
        entries.push_back(prerequisite);
        while entries.len() > MAX_RECENT_REPLAY_PREREQUISITES_PER_FAMILY {
            let _ = entries.pop_front();
        }
        Ok(())
    }

    fn maybe_append_test_completion_log(
        &self,
        family: &str,
        entry: &TestCompletionLogEntry,
    ) -> Result<(), GitAiError> {
        let Some(dir) = self.test_completion_log_dir.as_ref() else {
            return Ok(());
        };
        let _guard = self
            .test_completion_log_lock
            .lock()
            .map_err(|_| GitAiError::Generic("test completion log lock poisoned".to_string()))?;

        fs::create_dir_all(dir)?;
        let mut hasher = Sha256::new();
        hasher.update(family.as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        let path = dir.join(format!("{}.jsonl", &digest[..16]));
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let line = serde_json::to_string(entry).map_err(GitAiError::from)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }

    fn append_command_completion_log(
        &self,
        family: &str,
        applied: &crate::daemon::domain::AppliedCommand,
        result: &Result<(), GitAiError>,
        error_order: u64,
    ) -> Result<(), GitAiError> {
        let sync_tracked = crate::daemon::test_sync::tracks_primary_command_for_test_sync(
            applied.command.primary_command.as_deref(),
            &applied.command.invoked_args,
        );
        let test_sync_session = crate::daemon::test_sync::test_sync_session_from_invocation(
            &parsed_invocation_for_normalized_command(&applied.command),
        );
        let log_entry = TestCompletionLogEntry {
            seq: applied.seq,
            family_key: family.to_string(),
            kind: "command".to_string(),
            primary_command: applied.command.primary_command.clone(),
            test_sync_session,
            exit_code: Some(applied.command.exit_code),
            sync_tracked,
            status: if result.is_ok() {
                "ok".to_string()
            } else {
                "error".to_string()
            },
            error: result.as_ref().err().map(|error| error.to_string()),
        };
        if let Err(error) = self.maybe_append_test_completion_log(family, &log_entry) {
            let _ = self.record_side_effect_error(family, error_order, &error);
            return Err(error);
        }
        Ok(())
    }

    fn trace_root_is_tracked(ingress: &TraceIngressState, root: &str) -> bool {
        ingress.root_worktrees.contains_key(root)
            || ingress.root_families.contains_key(root)
            || ingress.root_argv.contains_key(root)
            || ingress.root_pre_repo.contains_key(root)
            || ingress.root_mutating.contains_key(root)
            || ingress.root_target_repo_only.contains_key(root)
            || ingress.root_reflog_refs.contains_key(root)
            || ingress.root_head_reflog_start_offsets.contains_key(root)
            || ingress.root_family_reflog_start_offsets.contains_key(root)
    }

    fn mark_trace_root_activity(&self, root_sid: &str) -> Result<(), GitAiError> {
        let mut ingress = self
            .trace_ingress_state
            .lock()
            .map_err(|_| GitAiError::Generic("trace ingress state lock poisoned".to_string()))?;
        ingress
            .root_last_activity_ns
            .insert(root_sid.to_string(), now_unix_nanos() as u64);
        ingress.root_close_fallback_enqueued.remove(root_sid);
        Ok(())
    }

    fn trace_root_connection_opened(&self, root_sid: &str) -> Result<(), GitAiError> {
        let mut ingress = self
            .trace_ingress_state
            .lock()
            .map_err(|_| GitAiError::Generic("trace ingress state lock poisoned".to_string()))?;
        *ingress
            .root_open_connections
            .entry(root_sid.to_string())
            .or_insert(0) += 1;
        Ok(())
    }

    fn record_trace_connection_close(&self, roots: &[String]) -> Result<Vec<String>, GitAiError> {
        let mut ingress = self
            .trace_ingress_state
            .lock()
            .map_err(|_| GitAiError::Generic("trace ingress state lock poisoned".to_string()))?;
        let mut stale_roots = Vec::new();
        for root_sid in roots {
            if let Some(count) = ingress.root_open_connections.get_mut(root_sid) {
                if *count > 1 {
                    *count -= 1;
                    continue;
                }
                ingress.root_open_connections.remove(root_sid);
            }
            stale_roots.push(root_sid.clone());
        }
        Ok(stale_roots)
    }

    fn trace_payload_root_sid(payload: &Value) -> Option<String> {
        payload
            .get("sid")
            .and_then(Value::as_str)
            .map(|sid| trace_root_sid(sid).to_string())
    }

    fn record_trace_payload_enqueued(&self, payload: &Value) -> Result<(), GitAiError> {
        self.record_trace_payload_enqueued_root(Self::trace_payload_root_sid(payload).as_deref())
    }

    fn record_trace_payload_enqueued_root(&self, root_sid: Option<&str>) -> Result<(), GitAiError> {
        let Some(root_sid) = root_sid else {
            return Ok(());
        };
        let mut queued = self.queued_trace_payloads_by_root.lock().map_err(|_| {
            GitAiError::Generic("queued trace payloads by root lock poisoned".to_string())
        })?;
        *queued.entry(root_sid.to_string()).or_insert(0) += 1;
        Ok(())
    }

    fn record_trace_payload_processed_root(
        &self,
        root_sid: Option<&str>,
    ) -> Result<(), GitAiError> {
        let Some(root_sid) = root_sid else {
            return Ok(());
        };
        let mut queued = self.queued_trace_payloads_by_root.lock().map_err(|_| {
            GitAiError::Generic("queued trace payloads by root lock poisoned".to_string())
        })?;
        if let Some(count) = queued.get_mut(root_sid) {
            if *count > 1 {
                *count -= 1;
            } else {
                queued.remove(root_sid);
            }
        }
        Ok(())
    }

    fn enqueue_stale_connection_close_fallbacks(&self, roots: &[String]) -> Result<(), GitAiError> {
        let stale_roots = {
            let mut ingress = self.trace_ingress_state.lock().map_err(|_| {
                GitAiError::Generic("trace ingress state lock poisoned".to_string())
            })?;
            let mut stale = Vec::new();
            for root_sid in roots {
                if !Self::trace_root_is_tracked(&ingress, root_sid) {
                    continue;
                }
                if ingress
                    .root_open_connections
                    .get(root_sid)
                    .copied()
                    .unwrap_or(0)
                    > 0
                {
                    continue;
                }
                if ingress.root_close_fallback_enqueued.contains(root_sid) {
                    continue;
                }
                ingress
                    .root_close_fallback_enqueued
                    .insert(root_sid.clone());
                stale.push(root_sid.clone());
            }
            stale
        };

        for root_sid in stale_roots {
            let mut payload = json!({
                "event": "atexit",
                "sid": root_sid,
                "code": 0,
                "time_ns": now_unix_nanos() as u64,
                "git_ai_connection_close_fallback": true,
            });
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    TRACE_INGEST_SEQ_FIELD.to_string(),
                    json!(self.next_trace_ingest_seq()),
                );
            }
            tracing::debug!(
                sid = %root_sid,
                "trace connection close fallback finalized"
            );
            self.enqueue_trace_payload(payload)?;
        }
        Ok(())
    }

    fn clear_trace_root_tracking(&self, root_sid: &str) -> Result<(), GitAiError> {
        let mut ingress = self
            .trace_ingress_state
            .lock()
            .map_err(|_| GitAiError::Generic("trace ingress state lock poisoned".to_string()))?;
        ingress.root_worktrees.remove(root_sid);
        ingress.root_families.remove(root_sid);
        ingress.root_argv.remove(root_sid);
        ingress.root_pre_repo.remove(root_sid);
        ingress.root_inflight_merge_squash_contexts.remove(root_sid);
        ingress.root_terminal_merge_squash_contexts.remove(root_sid);
        ingress.root_mutating.remove(root_sid);
        ingress.root_target_repo_only.remove(root_sid);
        ingress.root_reflog_refs.remove(root_sid);
        ingress.root_head_reflog_start_offsets.remove(root_sid);
        ingress.root_family_reflog_start_offsets.remove(root_sid);
        ingress.root_last_activity_ns.remove(root_sid);
        ingress.root_definitely_read_only.remove(root_sid);
        ingress.root_open_connections.remove(root_sid);
        ingress.root_close_fallback_enqueued.remove(root_sid);
        let mut queued = self.queued_trace_payloads_by_root.lock().map_err(|_| {
            GitAiError::Generic("queued trace payloads by root lock poisoned".to_string())
        })?;
        queued.remove(root_sid);
        Ok(())
    }

    fn discard_carryover_snapshots_for_root(&self, root_sid: &str) -> Result<(), GitAiError> {
        let snapshot_ids = self
            .carryover_snapshot_ids_by_root
            .lock()
            .map_err(|_| {
                GitAiError::Generic("carryover snapshot root map lock poisoned".to_string())
            })?
            .remove(root_sid)
            .unwrap_or_default();
        if !snapshot_ids.is_empty() {
            let mut snapshots = self.carryover_snapshots_by_id.lock().map_err(|_| {
                GitAiError::Generic("carryover snapshot store lock poisoned".to_string())
            })?;
            for snapshot_id in snapshot_ids {
                snapshots.remove(&snapshot_id);
            }
        }
        Ok(())
    }

    fn store_carryover_snapshot(
        &self,
        root_sid: &str,
        snapshot: HashMap<String, String>,
    ) -> Result<Option<String>, GitAiError> {
        if snapshot.is_empty() {
            return Ok(None);
        }

        let snapshot_id = format!(
            "{}-{}",
            now_unix_nanos(),
            // Relaxed: just a monotone counter for unique IDs; no cross-atomic ordering needed.
            self.next_carryover_snapshot_id
                .fetch_add(1, Ordering::Relaxed)
        );
        self.carryover_snapshots_by_id
            .lock()
            .map_err(|_| GitAiError::Generic("carryover snapshot store lock poisoned".to_string()))?
            .insert(snapshot_id.clone(), snapshot);
        self.carryover_snapshot_ids_by_root
            .lock()
            .map_err(|_| {
                GitAiError::Generic("carryover snapshot root map lock poisoned".to_string())
            })?
            .entry(root_sid.to_string())
            .or_insert_with(Vec::new)
            .push(snapshot_id.clone());
        Ok(Some(snapshot_id))
    }

    fn take_carryover_snapshot(
        &self,
        root_sid: &str,
        snapshot_id: &str,
    ) -> Result<Option<HashMap<String, String>>, GitAiError> {
        if let Ok(mut root_map) = self.carryover_snapshot_ids_by_root.lock()
            && let Some(ids) = root_map.get_mut(root_sid)
        {
            ids.retain(|existing| existing != snapshot_id);
            if ids.is_empty() {
                root_map.remove(root_sid);
            }
        }
        self.carryover_snapshots_by_id
            .lock()
            .map_err(|_| GitAiError::Generic("carryover snapshot store lock poisoned".to_string()))
            .map(|mut store| store.remove(snapshot_id))
    }

    fn capture_carryover_snapshot_for_command(
        &self,
        input: CarryoverCaptureInput<'_>,
    ) -> Result<Option<String>, GitAiError> {
        let parsed = parse_git_cli_args(trace_invocation_args(input.argv));
        let command = parsed.command.as_deref().or(input.primary_command);
        let Some(command) = command else {
            return Ok(None);
        };

        // `checkout/switch --merge` exits with code 1 when it produces conflict
        // markers, but HEAD still moves to the new branch.  The daemon requires a
        // carryover snapshot for such commands, so we must not bail out early on
        // non-zero exit here.  All other commands with non-zero exit produce no
        // meaningful state transition and need no snapshot.
        let is_merge_checkout = (command == "checkout" || command == "switch")
            && (parsed.has_command_flag("--merge") || parsed.has_command_flag("-m"));
        if input.exit_code != 0 && !is_merge_checkout {
            return Ok(None);
        }

        // Repo-creating commands (clone, init) have no meaningful carryover
        // state — the target repo doesn't exist before the command runs, and the
        // worktree hint may point to the CWD (a non-repo directory) rather than
        // the newly created repo.
        if matches!(command, "clone" | "init") {
            return Ok(None);
        }

        let repo = discover_repository_in_path_no_git_exec(input.worktree)?;
        let stable_heads = stable_carryover_heads_for_command(&repo, &input, &parsed)?;

        let mut file_paths = HashSet::new();
        match command {
            "commit" => {
                let (old_head, _) = stable_heads.clone().ok_or_else(|| {
                    GitAiError::Generic(format!(
                        "commit missing stable carryover heads sid={}",
                        input.root_sid
                    ))
                })?;
                file_paths.extend(tracked_working_log_files(&repo, &old_head)?);
            }
            "rebase" | "pull" => {
                if let Some((old_head, new_head)) = stable_heads.clone() {
                    if !old_head.is_empty() && !new_head.is_empty() && old_head != new_head {
                        file_paths.extend(tracked_working_log_files(&repo, &old_head)?);
                    }
                } else if command == "rebase" {
                    return Err(GitAiError::Generic(format!(
                        "rebase missing stable carryover heads sid={}",
                        input.root_sid
                    )));
                }
            }
            "checkout" | "switch" => {
                let is_merge = parsed.has_command_flag("--merge") || parsed.has_command_flag("-m");
                if is_merge
                    && let Some((old_head, new_head)) = stable_heads.clone()
                    && !old_head.is_empty()
                    && !new_head.is_empty()
                    && old_head != new_head
                {
                    file_paths.extend(tracked_working_log_files(&repo, &old_head)?);
                }
            }
            "reset" => {
                if !parsed.has_command_flag("--hard")
                    && let Some((old_head, _new_head)) = stable_heads.clone()
                    && !old_head.is_empty()
                {
                    file_paths.extend(tracked_working_log_files(&repo, &old_head)?);
                    let pathspecs = parsed.pathspecs();
                    if !pathspecs.is_empty() {
                        file_paths.retain(|file| matches_any_pathspec(file, &pathspecs));
                    }
                }
            }
            _ => {}
        }

        if file_paths.is_empty() {
            return Ok(None);
        }

        let snapshot = read_worktree_snapshot_for_files_at_or_before(
            input.worktree,
            &file_paths,
            input.finished_at_ns,
        );
        self.store_carryover_snapshot(input.root_sid, snapshot)
    }

    fn next_trace_ingest_seq(&self) -> u64 {
        // Relaxed: we only need fetch_add atomicity (unique monotone values),
        // not ordering w.r.t. any other atomic.
        (self.next_trace_ingest_seq.fetch_add(1, Ordering::Relaxed) as u64) + 1
    }

    fn start_trace_ingest_worker(self: &Arc<Self>) -> Result<(), GitAiError> {
        // Idempotent: if OnceLock is already set, worker is already running.
        if self.trace_ingest_tx.get().is_some() {
            return Ok(());
        }

        const TRACE_INGEST_QUEUE_CAPACITY: usize = 16_384;
        let (tx, mut rx) = mpsc::channel::<Value>(TRACE_INGEST_QUEUE_CAPACITY);
        // OnceLock::set fails if another thread raced us to initialize — that
        // means the worker is already running; just drop our channel ends.
        if self.trace_ingest_tx.set(tx).is_err() {
            return Ok(());
        }

        let coordinator = self.clone();
        tokio::spawn(async move {
            let mut next_seq: u64 = 1;
            let mut pending_by_seq: BTreeMap<u64, Value> = BTreeMap::new();
            let mut gc_counter: u64 = 0;
            const GC_INTERVAL: u64 = 500;

            // Previously: `while let Some(payload) = rx.recv().await { … }`
            //
            // The ingest worker used to exit when the sender was dropped by
            // `request_shutdown`.  With OnceLock the sender is never dropped
            // during the coordinator's lifetime, so we use select! to also
            // respond to the explicit shutdown signal.
            loop {
                let payload = tokio::select! {
                    biased; // prefer draining queued work over shutdown
                    maybe = rx.recv() => match maybe {
                        Some(p) => p,
                        None => break, // channel closed (coordinator dropped)
                    },
                    _ = coordinator.wait_for_shutdown() => break,
                };
                let Some(seq) = payload.get(TRACE_INGEST_SEQ_FIELD).and_then(Value::as_u64) else {
                    tracing::error!(
                        component = "daemon",
                        phase = "trace_ingest_worker",
                        reason = "missing_ingest_seq",
                        "trace ingest payload missing ingress sequence"
                    );
                    coordinator.request_shutdown();
                    break;
                };

                if pending_by_seq.len() >= TRACE_INGEST_QUEUE_CAPACITY {
                    tracing::error!(
                        component = "daemon",
                        phase = "trace_ingest_worker",
                        reason = "reorder_buffer_overflow",
                        buffered_count = pending_by_seq.len(),
                        next_seq,
                        received_seq = seq,
                        "trace ingest reorder buffer overflow"
                    );
                    coordinator.request_shutdown();
                    break;
                }

                if pending_by_seq.insert(seq, payload).is_some() {
                    tracing::error!(
                        component = "daemon",
                        phase = "trace_ingest_worker",
                        reason = "duplicate_ingest_seq",
                        sequence = seq,
                        "duplicate trace ingest sequence received"
                    );
                    coordinator.request_shutdown();
                    break;
                }

                while let Some(mut ordered_payload) = pending_by_seq.remove(&next_seq) {
                    let processed_seq = next_seq;
                    if let Some(object) = ordered_payload.as_object_mut() {
                        object.remove(TRACE_INGEST_SEQ_FIELD);
                    }
                    let ordered_payload_root = Self::trace_payload_root_sid(&ordered_payload);

                    let ingest_result = {
                        let coord = coordinator.clone();
                        let future = coord.ingest_trace_payload_fast(ordered_payload);
                        let caught = std::panic::AssertUnwindSafe(future);
                        match futures::FutureExt::catch_unwind(caught).await {
                            Ok(Ok(())) => Ok(()),
                            Ok(Err(error)) => {
                                tracing::error!(
                                    component = "daemon",
                                    phase = "trace_ingest_worker",
                                    reason = "ingest_error",
                                    sequence = processed_seq,
                                    root_sid = ?ordered_payload_root,
                                    %error,
                                    "trace ingest error"
                                );
                                Err(error)
                            }
                            Err(panic_payload) => {
                                let panic_msg =
                                    if let Some(s) = panic_payload.downcast_ref::<String>() {
                                        s.clone()
                                    } else if let Some(s) = panic_payload.downcast_ref::<&str>() {
                                        s.to_string()
                                    } else {
                                        "unknown panic".to_string()
                                    };
                                tracing::error!(
                                    component = "daemon",
                                    phase = "trace_ingest_worker",
                                    reason = "panic_in_ingest",
                                    panic_msg = %panic_msg,
                                    sequence = processed_seq,
                                    "trace ingest panic"
                                );
                                Err(GitAiError::Generic(format!(
                                    "trace ingest worker panic: {}",
                                    panic_msg
                                )))
                            }
                        }
                    };
                    let _ = ingest_result;
                    let _ = coordinator.queued_trace_payloads.fetch_update(
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                        |current| Some(current.saturating_sub(1)),
                    );
                    if let Err(error) = coordinator
                        .record_trace_payload_processed_root(ordered_payload_root.as_deref())
                    {
                        tracing::debug!(
                            %error,
                            "trace payload accounting error after ingest"
                        );
                    }
                    // Release: pairs with Acquire loads in wait_for_trace_ingest_processed_through
                    // so waiters observe all ingest side-effects when seq advances.
                    coordinator
                        .processed_trace_ingest_seq
                        .store(processed_seq as usize, Ordering::Release);
                    coordinator.trace_ingest_progress_notify.notify_waiters();
                    next_seq = next_seq.saturating_add(1);
                    gc_counter += 1;
                    if gc_counter.is_multiple_of(GC_INTERVAL) {
                        coordinator.gc_stale_family_state();
                    }
                }
            }

            if !pending_by_seq.is_empty() {
                tracing::error!(
                    component = "daemon",
                    phase = "trace_ingest_worker",
                    reason = "unflushed_buffer_on_shutdown",
                    buffered_count = pending_by_seq.len(),
                    next_seq,
                    min_buffered_seq = ?pending_by_seq.keys().next().copied(),
                    max_buffered_seq = ?pending_by_seq.keys().last().copied(),
                    "trace ingest worker exiting with buffered out-of-order frames"
                );
            }
        });
        Ok(())
    }

    fn enqueue_trace_payload(&self, payload: Value) -> Result<(), GitAiError> {
        let tx =
            self.trace_ingest_tx.get().cloned().ok_or_else(|| {
                GitAiError::Generic("trace ingest worker not started".to_string())
            })?;
        let payload_root = Self::trace_payload_root_sid(&payload);
        self.record_trace_payload_enqueued(&payload)?;
        // Relaxed: this counter tracks in-flight count for monitoring; no
        // ordering dependency with any other atomic.
        self.queued_trace_payloads.fetch_add(1, Ordering::Relaxed);
        let send_result = match tx.try_send(payload) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_payload)) => Err(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(payload)) => {
                if tokio::runtime::Handle::try_current().is_ok() {
                    tokio::task::block_in_place(|| tx.blocking_send(payload)).map_err(|_| ())
                } else {
                    tx.blocking_send(payload).map_err(|_| ())
                }
            }
        };
        if send_result.is_err() {
            let _ = self.queued_trace_payloads.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |current| Some(current.saturating_sub(1)),
            );
            if let Err(error) = self.record_trace_payload_processed_root(payload_root.as_deref()) {
                tracing::debug!(
                    %error,
                    "trace payload accounting rollback error"
                );
            }
            tracing::error!(
                component = "daemon",
                phase = "enqueue_trace_payload",
                reason = "ingest_worker_channel_closed",
                "trace ingest queue send failed: worker may have crashed"
            );
            self.request_shutdown();
            return Err(GitAiError::Generic(
                "trace ingest queue send failed: worker may have crashed".to_string(),
            ));
        }
        Ok(())
    }

    /// Waits until all trace payloads enqueued up to now have been processed
    /// by the ingest worker. This is a causal drain fence: it guarantees that
    /// any trace2 event already in the ingest queue (e.g., from a `git reset`
    /// that exited before this function was called) has been fully processed
    /// before returning.
    ///
    /// Used by checkpoint entry to ensure ordering: a checkpoint must not be
    /// processed until all causally-prior git operations have been ingested.
    async fn wait_for_trace_ingest_processed_through(&self) {
        // Read the current high-water mark. Any payload enqueued before this
        // point has a seq <= this value. We need to wait until the ingest
        // worker has processed through at least this seq.
        let target_seq = self.next_trace_ingest_seq.load(Ordering::Acquire) as u64;
        if target_seq == 0 {
            return;
        }
        // The target is (next - 1) because next_trace_ingest_seq is pre-incremented
        // by fetch_add before use, so the last *assigned* seq is (current_value - 1).
        // But since we loaded AFTER the fetch_add that assigned the seq, we need to
        // check processed_seq >= target_seq - 1 (the last allocated seq).
        let target = target_seq.saturating_sub(1);
        loop {
            let processed = self.processed_trace_ingest_seq.load(Ordering::Acquire) as u64;
            if processed >= target {
                return;
            }
            self.trace_ingest_progress_notify.notified().await;
        }
    }

    /// Prepares `payload` for ingestion and returns whether it should be
    /// enqueued.
    ///
    /// - `true`  — payload is for a mutating command; a sequence number has
    ///   been stamped and the caller MUST call `enqueue_trace_payload`.
    /// - `false` — payload is for a definitely-read-only invocation; it was
    ///   handled inline and the caller MUST NOT enqueue it.
    ///
    /// Sequence numbers are only allocated for payloads that will be enqueued,
    /// so the `processed_trace_ingest_seq` watermark (used by checkpoint
    /// `wait_for_trace_ingest_processed_through`) advances without gaps.
    pub(crate) fn prepare_trace_payload_for_ingest(&self, payload: &mut Value) -> bool {
        // Check read-only status BEFORE allocating a sequence number so that
        // read-only invocations never perturb the ingest sequence counter.
        let is_read_only = self.augment_trace_payload_with_reflog_metadata(payload);
        if is_read_only {
            return false;
        }
        // Mutating command: stamp a sequence number so the ingest worker can
        // reorder out-of-order events from concurrent git invocations.
        if let Some(object) = payload.as_object_mut()
            && object.get(TRACE_INGEST_SEQ_FIELD).is_none()
        {
            object.insert(
                TRACE_INGEST_SEQ_FIELD.to_string(),
                json!(self.next_trace_ingest_seq()),
            );
        }
        true
    }

    /// Augments `payload` with pre/post repository state and reflog metadata
    /// needed by the ingest worker.
    ///
    /// Returns `true` when the payload belongs to a definitely-read-only
    /// invocation (e.g. `git status`, `git stash list`, `git worktree list`).
    /// In that case the caller must **not** enqueue the payload — all required
    /// bookkeeping has already been performed inline here, and routing the
    /// event through the serial ingest queue would create unnecessary backlog
    /// when IDEs fire dozens of read-only commands per second (the Zed IDE
    /// was observed generating >40 such invocations/sec, flooding the daemon
    /// with 120–415 trace events/sec and causing >1 min backlog).
    ///
    /// Returns `false` for mutating or unknown commands: the caller should
    /// stamp a sequence number and enqueue the payload normally.
    fn augment_trace_payload_with_reflog_metadata(&self, payload: &mut Value) -> bool {
        let event = payload
            .get("event")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let sid = payload
            .get("sid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if sid.is_empty() {
            return false;
        }

        let root = trace_root_sid(&sid).to_string();

        // Fast path: for invocations that are definitively read-only (status,
        // diff, stash list, worktree list, …) skip all expensive filesystem
        // I/O (worktree resolution, HEAD state reads, reflog captures) and do
        // only lightweight bookkeeping.  The caller will NOT enqueue these
        // payloads, keeping the serial ingest queue exclusively for mutating
        // commands.
        let argv = trace_payload_argv(payload);
        let early_primary =
            trace_payload_primary_command(payload).or_else(|| trace_argv_primary_command(&argv));
        // Extend the read-only check to cover subcommand-gated cases such as
        // `stash list` and `worktree list` that would otherwise fall through
        // to the expensive full path.
        let event_is_read_only =
            trace_invocation_is_definitely_read_only(early_primary.as_deref(), &argv);
        // For events with no command info (exit/atexit), defer to the cached
        // flag inside the lock to avoid a second lock acquisition.
        let may_be_read_only = event_is_read_only || early_primary.is_none();
        if may_be_read_only {
            let mut ingress = match self.trace_ingress_state.lock() {
                Ok(guard) => guard,
                // If the lock is poisoned we cannot determine read-only status;
                // fall through and let the ingest worker handle error recovery.
                Err(_) => return false,
            };
            // If the event itself wasn't identified as read-only, check the root flag.
            if !event_is_read_only && !ingress.root_definitely_read_only.contains(&root) {
                // Not read-only — drop the lock and fall through to the full path.
                drop(ingress);
            } else {
                // Activity tracking (folded here to avoid a separate lock acquisition)
                ingress
                    .root_last_activity_ns
                    .insert(root.clone(), now_unix_nanos() as u64);
                ingress.root_close_fallback_enqueued.remove(&root);
                // Minimal state tracking for connection lifecycle
                if let Some(worktree) = trace_payload_worktree_hint(payload) {
                    ingress.root_worktrees.insert(root.clone(), worktree);
                }
                if event == "start" && sid == root && !argv.is_empty() {
                    ingress.root_argv.insert(root.clone(), argv);
                    ingress.root_definitely_read_only.insert(root.clone());
                }
                ingress.root_mutating.entry(root.clone()).or_insert(false);
                // Cleanup on terminal event
                if is_terminal_root_trace_event(&event, &sid, &root) {
                    ingress.root_families.remove(&root);
                    ingress.root_mutating.remove(&root);
                    ingress.root_target_repo_only.remove(&root);
                    ingress.root_argv.remove(&root);
                    ingress.root_pre_repo.remove(&root);
                    ingress.root_worktrees.remove(&root);
                    ingress.root_inflight_merge_squash_contexts.remove(&root);
                    ingress.root_terminal_merge_squash_contexts.remove(&root);
                    ingress.root_reflog_refs.remove(&root);
                    ingress.root_head_reflog_start_offsets.remove(&root);
                    ingress.root_family_reflog_start_offsets.remove(&root);
                    ingress.root_last_activity_ns.remove(&root);
                    ingress.root_definitely_read_only.remove(&root);
                }
                // Payload was fully handled inline; tell the caller to skip enqueue.
                return true;
            }
        }

        let _ = self.mark_trace_root_activity(&root);
        let mut ingress = match self.trace_ingress_state.lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::error!(
                    component = "daemon",
                    phase = "augment_trace_payload_with_reflog_metadata",
                    %sid,
                    %event,
                    "trace ingress state lock poisoned"
                );
                return false;
            }
        };

        if let Some(worktree) = trace_payload_worktree_hint(payload) {
            if let Some(common_dir) = common_dir_for_worktree(&worktree) {
                let family = common_dir.canonicalize().unwrap_or(common_dir);
                ingress
                    .root_families
                    .insert(root.clone(), family.to_string_lossy().to_string());
            }
            ingress.root_worktrees.insert(root.clone(), worktree);
        }
        let payload_argv = trace_payload_argv(payload);
        if event == "start" && sid == root && !payload_argv.is_empty() {
            ingress.root_argv.insert(root.clone(), payload_argv.clone());
        }
        let effective_argv = if payload_argv.is_empty() {
            ingress.root_argv.get(&root).cloned().unwrap_or_default()
        } else {
            payload_argv
        };
        let effective_primary = trace_payload_primary_command(payload)
            .or_else(|| trace_argv_primary_command(&effective_argv));
        if let Some(primary) = effective_primary.clone() {
            let should_capture = trace_command_may_mutate_refs(Some(primary.as_str()));
            match ingress.root_mutating.get(&root).copied() {
                Some(false) if should_capture => {
                    ingress.root_mutating.insert(root.clone(), true);
                }
                None => {
                    ingress.root_mutating.insert(root.clone(), should_capture);
                }
                _ => {}
            }
            let target_repo_only =
                trace_command_uses_target_repo_context_only(Some(primary.as_str()));
            match ingress.root_target_repo_only.get(&root).copied() {
                Some(false) if target_repo_only => {
                    ingress.root_target_repo_only.insert(root.clone(), true);
                    ingress.root_reflog_refs.remove(&root);
                    ingress.root_head_reflog_start_offsets.remove(&root);
                    ingress.root_family_reflog_start_offsets.remove(&root);
                }
                None => {
                    ingress
                        .root_target_repo_only
                        .insert(root.clone(), target_repo_only);
                }
                _ => {}
            }
        }

        let Some(worktree) = ingress.root_worktrees.get(&root).cloned() else {
            if is_terminal_root_trace_event(&event, &sid, &root) {
                ingress.root_families.remove(&root);
                ingress.root_mutating.remove(&root);
                ingress.root_target_repo_only.remove(&root);
                ingress.root_argv.remove(&root);
                ingress.root_pre_repo.remove(&root);
                ingress.root_inflight_merge_squash_contexts.remove(&root);
                ingress.root_terminal_merge_squash_contexts.remove(&root);
                ingress.root_reflog_refs.remove(&root);
                ingress.root_head_reflog_start_offsets.remove(&root);
                ingress.root_family_reflog_start_offsets.remove(&root);
            }
            return false;
        };

        let should_capture_mutation = *ingress.root_mutating.get(&root).unwrap_or(&false);
        let target_repo_only = *ingress.root_target_repo_only.get(&root).unwrap_or(&false);
        if !target_repo_only
            && !ingress.root_pre_repo.contains_key(&root)
            && let Some(state) = read_head_state_for_worktree(&worktree)
        {
            ingress
                .root_pre_repo
                .insert(root.clone(), repo_context_from_head_state(state));
        }
        let pre_repo = ingress.root_pre_repo.get(&root).cloned();
        if should_capture_mutation && !target_repo_only {
            let contextual_refs = if let Some(repo) = pre_repo.as_ref() {
                tracked_reflog_refs_for_command(
                    effective_primary.as_deref(),
                    Some(repo),
                    &worktree,
                    &effective_argv,
                )
            } else {
                tracked_reflog_refs_for_command(
                    effective_primary.as_deref(),
                    None,
                    &worktree,
                    &effective_argv,
                )
            };
            let refs = ingress
                .root_reflog_refs
                .entry(root.clone())
                .or_insert_with(Vec::new);
            for reference in contextual_refs {
                if !refs.iter().any(|existing| existing == &reference) {
                    refs.push(reference);
                }
            }
            refs.sort();
            refs.dedup();
        }
        let cached_inflight_merge_squash = ingress
            .root_inflight_merge_squash_contexts
            .get(&root)
            .cloned();
        let cached_terminal_merge_squash = ingress
            .root_terminal_merge_squash_contexts
            .get(&root)
            .cloned();
        drop(ingress);

        let mut inflight_merge_squash_to_cache = None;
        if let Some(object) = payload.as_object_mut() {
            if let Some(repo) = pre_repo.as_ref() {
                object.insert("git_ai_pre_repo".to_string(), json!(repo));
            }
            if object.get("git_ai_merge_squash_source_head").is_none() {
                let inflight_merge_squash = if let Some(context) = cached_inflight_merge_squash {
                    Ok(Some(context))
                } else {
                    capture_inflight_merge_squash_source_head_for_commit(
                        &worktree,
                        effective_primary.as_deref(),
                        &effective_argv,
                    )
                };
                match inflight_merge_squash {
                    Ok(Some(source_head)) => {
                        inflight_merge_squash_to_cache = Some(source_head.clone());
                        object.insert(
                            "git_ai_merge_squash_source_head".to_string(),
                            json!(source_head),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::error!(
                            component = "daemon",
                            phase = "augment_trace_payload_with_reflog_metadata",
                            root_sid = %root,
                            %sid,
                            ?effective_argv,
                            %error,
                            "commit squash context capture failed"
                        );
                    }
                }
            }
            if object.get("git_ai_stash_target_oid").is_none()
                && object.get("git_ai_stash_target_oid_error").is_none()
            {
                match resolve_stash_target_oid_for_command(&worktree, &effective_argv) {
                    Ok(Some(stash_target_oid)) => {
                        object.insert(
                            "git_ai_stash_target_oid".to_string(),
                            json!(stash_target_oid),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::error!(
                            component = "daemon",
                            phase = "augment_trace_payload_with_reflog_metadata",
                            root_sid = %root,
                            %sid,
                            ?effective_argv,
                            %error,
                            "stash target resolution failed"
                        );
                        object.insert(
                            "git_ai_stash_target_oid_error".to_string(),
                            json!(error.to_string()),
                        );
                    }
                }
            }
        }

        let terminal_exit_code = if is_terminal_root_trace_event(&event, &sid, &root) {
            Some(
                payload
                    .get("code")
                    .or_else(|| payload.get("exit_code"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0) as i32,
            )
        } else {
            None
        };
        let post_repo = if terminal_exit_code.is_some() {
            read_head_state_for_worktree(&worktree).map(repo_context_from_head_state)
        } else {
            None
        };
        let mut terminal_merge_squash_to_cache = None;
        if is_terminal_root_trace_event(&event, &sid, &root)
            && let Some(object) = payload.as_object_mut()
        {
            if let Some(state) = post_repo.as_ref() {
                object.insert("git_ai_post_repo".to_string(), json!(state));
            }

            let terminal_merge_squash = if let Some(context) = cached_terminal_merge_squash {
                Ok(Some(context))
            } else {
                capture_merge_squash_source_head_for_command(
                    &worktree,
                    effective_primary.as_deref(),
                    &effective_argv,
                    terminal_exit_code.unwrap_or(0),
                )
            };

            match terminal_merge_squash {
                Ok(Some(source_head)) => {
                    terminal_merge_squash_to_cache = Some(source_head.clone());
                    object.insert(
                        "git_ai_merge_squash_source_head".to_string(),
                        json!(source_head),
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::error!(
                        component = "daemon",
                        phase = "augment_trace_payload_with_reflog_metadata",
                        root_sid = %root,
                        %sid,
                        ?effective_argv,
                        %error,
                        "merge --squash context capture failed"
                    );
                }
            }
        }

        let mut ingress = match self.trace_ingress_state.lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::error!(
                    component = "daemon",
                    phase = "augment_trace_payload_with_reflog_metadata",
                    %sid,
                    %event,
                    "trace ingress state lock poisoned"
                );
                return false;
            }
        };
        if let Some(context) = inflight_merge_squash_to_cache {
            ingress
                .root_inflight_merge_squash_contexts
                .entry(root.clone())
                .or_insert(context);
        }
        if let Some(context) = terminal_merge_squash_to_cache {
            ingress
                .root_terminal_merge_squash_contexts
                .entry(root.clone())
                .or_insert(context);
        }
        if should_capture_mutation && !target_repo_only {
            if !ingress.root_head_reflog_start_offsets.contains_key(&root)
                && let Some(offset) = daemon_worktree_head_reflog_offset(&worktree)
            {
                ingress
                    .root_head_reflog_start_offsets
                    .insert(root.clone(), offset);
            }
            if !ingress.root_family_reflog_start_offsets.contains_key(&root)
                && let Some(refs) = ingress.root_reflog_refs.get(&root)
                && let Some(offsets) = daemon_reflog_offsets_for_refs(&worktree, refs)
            {
                ingress
                    .root_family_reflog_start_offsets
                    .insert(root.clone(), offsets);
            }
        }

        if is_terminal_root_trace_event(&event, &sid, &root)
            && let Some(object) = payload.as_object_mut()
        {
            let mut terminal_ref_changes: Option<Vec<crate::daemon::domain::RefChange>> = None;
            if let Some(state) = post_repo.as_ref() {
                object.insert("git_ai_post_repo".to_string(), json!(state));
            }
            if should_capture_mutation && !target_repo_only {
                if let Some(start_offset) =
                    ingress.root_head_reflog_start_offsets.get(&root).copied()
                {
                    object.insert(
                        "git_ai_worktree_head_reflog_start".to_string(),
                        json!(start_offset),
                    );
                }
                if let Some(end_offset) = daemon_worktree_head_reflog_offset(&worktree) {
                    object.insert(
                        "git_ai_worktree_head_reflog_end".to_string(),
                        json!(end_offset),
                    );
                }
                if let Some(start_offsets) = ingress.root_family_reflog_start_offsets.get(&root) {
                    object.insert(
                        "git_ai_family_reflog_start".to_string(),
                        json!(start_offsets),
                    );
                    if let Some(refs) = ingress.root_reflog_refs.get(&root)
                        && let Some(mut end_offsets) =
                            daemon_reflog_offsets_for_refs(&worktree, refs)
                    {
                        for (reference, start_offset) in start_offsets {
                            let end_offset = end_offsets
                                .entry(reference.clone())
                                .or_insert(*start_offset);
                            if *end_offset < *start_offset {
                                *end_offset = *start_offset;
                            }
                        }
                        match daemon_reflog_delta_from_offsets(
                            &worktree,
                            start_offsets,
                            &end_offsets,
                        ) {
                            Ok(ref_changes) => {
                                object.insert(
                                    "git_ai_family_reflog_changes".to_string(),
                                    json!(&ref_changes),
                                );
                                terminal_ref_changes = Some(ref_changes);
                            }
                            Err(error) => {
                                tracing::debug!(
                                    %error,
                                    %sid,
                                    "trace reflog delta capture error"
                                );
                            }
                        }
                        object.insert("git_ai_family_reflog_end".to_string(), json!(end_offsets));
                    }
                }
            }
            if object.get("git_ai_stash_target_oid").is_none() {
                match resolve_stash_target_oid_for_terminal_payload(
                    &worktree,
                    &effective_argv,
                    terminal_ref_changes.as_deref().unwrap_or(&[]),
                ) {
                    Ok(Some(stash_target_oid)) => {
                        object.remove("git_ai_stash_target_oid_error");
                        object.insert(
                            "git_ai_stash_target_oid".to_string(),
                            json!(stash_target_oid),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::error!(
                            component = "daemon",
                            phase = "augment_trace_payload_with_reflog_metadata",
                            root_sid = %root,
                            %sid,
                            ?effective_argv,
                            %error,
                            "terminal stash target resolution failed"
                        );
                        object.insert(
                            "git_ai_stash_target_oid_error".to_string(),
                            json!(error.to_string()),
                        );
                    }
                }
            }
            if object.get("git_ai_carryover_snapshot_id").is_none() {
                let terminal_time_ns = object
                    .get("time")
                    .and_then(Value::as_str)
                    .and_then(rfc3339_to_unix_nanos)
                    .or_else(|| {
                        object
                            .get("time_ns")
                            .and_then(Value::as_u64)
                            .map(u128::from)
                    })
                    .or_else(|| object.get("ts").and_then(Value::as_u64).map(u128::from))
                    .or_else(|| {
                        object
                            .get("t_abs")
                            .and_then(Value::as_f64)
                            .and_then(|seconds| {
                                if seconds.is_sign_negative() {
                                    None
                                } else {
                                    Some((seconds * 1_000_000_000_f64) as u128)
                                }
                            })
                    })
                    .unwrap_or_else(now_unix_nanos);
                match self.capture_carryover_snapshot_for_command(CarryoverCaptureInput {
                    root_sid: &root,
                    worktree: &worktree,
                    primary_command: effective_primary.as_deref(),
                    argv: &effective_argv,
                    exit_code: terminal_exit_code.unwrap_or(0),
                    finished_at_ns: terminal_time_ns,
                    post_repo: post_repo.as_ref(),
                    ref_changes: terminal_ref_changes.as_deref().unwrap_or(&[]),
                }) {
                    Ok(Some(snapshot_id)) => {
                        object.insert(
                            "git_ai_carryover_snapshot_id".to_string(),
                            json!(snapshot_id),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::error!(
                            component = "daemon",
                            phase = "augment_trace_payload_with_reflog_metadata",
                            root_sid = %root,
                            %sid,
                            ?effective_argv,
                            %error,
                            "carryover snapshot capture failed"
                        );
                    }
                }
            }
            ingress.root_worktrees.remove(&root);
            ingress.root_families.remove(&root);
            ingress.root_argv.remove(&root);
            ingress.root_pre_repo.remove(&root);
            ingress.root_inflight_merge_squash_contexts.remove(&root);
            ingress.root_terminal_merge_squash_contexts.remove(&root);
            ingress.root_mutating.remove(&root);
            ingress.root_target_repo_only.remove(&root);
            ingress.root_reflog_refs.remove(&root);
            ingress.root_head_reflog_start_offsets.remove(&root);
            ingress.root_family_reflog_start_offsets.remove(&root);
        }
        // Payload was fully augmented for a mutating command; tell the caller
        // to stamp a sequence number and enqueue it.
        false
    }

    fn side_effect_exec_lock(&self, family: &str) -> Result<Arc<AsyncMutex<()>>, GitAiError> {
        let mut map = self
            .side_effect_exec_locks
            .lock()
            .map_err(|_| GitAiError::Generic("side effect lock map lock poisoned".to_string()))?;
        Ok(map
            .entry(family.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone())
    }

    async fn append_checkpoint_to_family_sequencer(
        &self,
        family: &str,
        request: CheckpointRequest,
        respond_to: Option<oneshot::Sender<Result<u64, GitAiError>>>,
    ) -> Result<(), GitAiError> {
        // Causal drain fence: ensure all trace2 events already in the ingest
        // queue have been processed before we insert this checkpoint. Without
        // this, a checkpoint can race ahead of a git reset/rebase trace2 event
        // and compute its diff against stale working-log state.
        self.wait_for_trace_ingest_processed_through().await;

        let exec_lock = self.side_effect_exec_lock(family)?;
        let _guard = exec_lock.lock().await;

        {
            let mut sequencers = self.family_sequencers_by_family.lock().map_err(|_| {
                GitAiError::Generic("family sequencer map lock poisoned".to_string())
            })?;
            let state =
                sequencers
                    .entry(family.to_string())
                    .or_insert_with(|| FamilySequencerState {
                        next_ordinal: 1,
                        entries: BTreeMap::new(),
                    });
            let order = FamilySequencerOrder {
                started_at_ns: now_unix_nanos(),
                ordinal: state.next_ordinal,
            };
            state.next_ordinal = state.next_ordinal.saturating_add(1);
            state.entries.insert(
                order,
                FamilySequencerEntry::Checkpoint {
                    request: Box::new(request),
                    respond_to,
                },
            );
        }

        self.drain_ready_family_sequencer_entries_locked(family)
            .await
    }

    async fn drain_ready_family_sequencer_entries_locked(
        &self,
        family: &str,
    ) -> Result<(), GitAiError> {
        let mut ready: Vec<(u64, FamilySequencerEntry)> = Vec::new();
        let mut progressed = false;
        {
            let mut map = self.family_sequencers_by_family.lock().map_err(|_| {
                GitAiError::Generic("family sequencer map lock poisoned".to_string())
            })?;
            let state = map
                .entry(family.to_string())
                .or_insert_with(|| FamilySequencerState {
                    next_ordinal: 1,
                    entries: BTreeMap::new(),
                });
            while let Some(first_entry) = state.entries.first_entry() {
                if matches!(first_entry.get(), FamilySequencerEntry::PendingRoot) {
                    break;
                }
                let (order, entry) = first_entry.remove_entry();
                match entry {
                    FamilySequencerEntry::PendingRoot => {
                        unreachable!("pending root should not be removed from sequencer front");
                    }
                    other => {
                        ready.push((order.ordinal, other));
                        progressed = true;
                    }
                }
            }
        }

        if ready.is_empty() {
            return Ok(());
        }

        let _ = self.begin_family_effect(family);
        for (order, ready_entry) in ready {
            match ready_entry {
                FamilySequencerEntry::ReadyCommand(command) => {
                    // Wrap the entire command + side-effect pipeline in catch_unwind
                    // so that a panic (e.g. from UTF-8 boundary issues in diff parsing)
                    // does not kill the daemon process.
                    let side_effect_result = {
                        let future = async {
                            let applied = self.coordinator.route_command(*command).await?;
                            let side_effect = self
                                .maybe_apply_side_effects_for_applied_command(
                                    Some(family),
                                    &applied,
                                )
                                .await;
                            Ok::<_, GitAiError>((applied, side_effect))
                        };
                        let caught = std::panic::AssertUnwindSafe(future);
                        futures::FutureExt::catch_unwind(caught).await
                    };
                    match side_effect_result {
                        Ok(Ok((applied, side_effect_result))) => {
                            if let Err(error) = &side_effect_result {
                                let _ = self.record_side_effect_error(family, order, error);
                                tracing::error!(
                                    %error,
                                    %family,
                                    seq = applied.seq,
                                    "command side effect failed"
                                );
                            }
                            if let Err(error) = self.append_command_completion_log(
                                family,
                                &applied,
                                &side_effect_result,
                                order,
                            ) {
                                let _ = self.record_side_effect_error(family, order, &error);
                                tracing::error!(
                                    %error,
                                    %family,
                                    order,
                                    "command completion log write failed"
                                );
                            }
                        }
                        Ok(Err(error)) => {
                            let _ = self.record_side_effect_error(family, order, &error);
                            tracing::error!(
                                %error,
                                %family,
                                order,
                                "command apply failed"
                            );
                        }
                        Err(panic_payload) => {
                            let panic_msg = if let Some(s) = panic_payload.downcast_ref::<String>()
                            {
                                s.clone()
                            } else if let Some(s) = panic_payload.downcast_ref::<&str>() {
                                s.to_string()
                            } else {
                                "unknown panic".to_string()
                            };
                            let error = GitAiError::Generic(format!(
                                "daemon command side effect panic: {}",
                                panic_msg
                            ));
                            let _ = self.record_side_effect_error(family, order, &error);
                            tracing::error!(
                                component = "daemon",
                                phase = "command_side_effect",
                                reason = "panic_in_side_effect",
                                panic_msg = %panic_msg,
                                %family,
                                order,
                                "command side effect panic"
                            );
                        }
                    }
                }
                FamilySequencerEntry::Checkpoint {
                    mut request,
                    respond_to,
                } => {
                    let repo_wd = request
                        .files
                        .first()
                        .map(|f| f.repo_work_dir.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let checkpoint_file_paths: Vec<String> = request
                        .files
                        .iter()
                        .map(|f| f.path.to_string_lossy().to_string())
                        .collect();
                    let checkpoint_kind = request.checkpoint_kind;
                    let checkpoint_path_role = request.path_role;
                    let checkpoint_has_agent = request.agent_id.is_some();
                    let checkpoint_kind_str = format!("{:?}", checkpoint_kind);
                    let is_human_checkpoint = checkpoint_kind == CheckpointKind::Human;

                    // Register pending AI edit state when an AI agent fires its
                    // pre-edit snapshot. This signals that an AI edit is in-flight.
                    // Identified by: WillEdit path_role + agent_id present (only AI
                    // agent presets have an agent_id on their pre-edit checkpoints).
                    if checkpoint_path_role == PreparedPathRole::WillEdit && checkpoint_has_agent {
                        self.register_pending_ai_edits(family, &checkpoint_file_paths);
                    }

                    // Filter out files with pending AI edits from KnownHuman checkpoints.
                    // These are spurious IDE save events that fire between pre/post-edit.
                    if checkpoint_kind == CheckpointKind::KnownHuman {
                        let pending_files: Vec<String> = checkpoint_file_paths
                            .iter()
                            .filter(|f| self.file_has_pending_ai_edit(family, f))
                            .cloned()
                            .collect();
                        if !pending_files.is_empty() {
                            request.files.retain(|f| {
                                let path_str = f.path.to_string_lossy().to_string();
                                !pending_files.contains(&path_str)
                            });
                            tracing::debug!(
                                "[KnownHuman] Filtered {} file(s) with pending AI edits",
                                pending_files.len()
                            );
                            if request.files.is_empty() {
                                let log_entry = TestCompletionLogEntry {
                                    seq: 0,
                                    family_key: family.to_string(),
                                    kind: "checkpoint".to_string(),
                                    primary_command: Some("checkpoint".to_string()),
                                    test_sync_session: None,
                                    exit_code: None,
                                    sync_tracked: true,
                                    status: "suppressed".to_string(),
                                    error: None,
                                };
                                let _ = self.maybe_append_test_completion_log(family, &log_entry);
                                if let Some(respond_to) = respond_to {
                                    let _ = respond_to.send(Ok(0));
                                }
                                continue;
                            }
                        }
                    }

                    // Recompute file paths after potential KnownHuman filtering so
                    // watermark computation and clear_pending_ai_edits use the actual
                    // files that will be checkpointed.
                    let checkpoint_file_paths: Vec<String> = request
                        .files
                        .iter()
                        .map(|f| f.path.to_string_lossy().to_string())
                        .collect();

                    let should_log_completion = true; // Always log for test sync
                    tracing::info!(kind = %checkpoint_kind_str, repo = %repo_wd, "checkpoint start");
                    let checkpoint_start = std::time::Instant::now();
                    let checkpoint_request = {
                        let future = async {
                            if !repo_wd.is_empty() {
                                let ack =
                                    self.coordinator.apply_checkpoint(Path::new(&repo_wd)).await;
                                match ack {
                                    Ok(ack) => {
                                        apply_checkpoint_side_effect(*request).map(|_| ack.seq)
                                    }
                                    Err(error) => Err(error),
                                }
                            } else {
                                apply_checkpoint_side_effect(*request).map(|_| 0)
                            }
                        };
                        let caught = std::panic::AssertUnwindSafe(future);
                        futures::FutureExt::catch_unwind(caught).await
                    };
                    let result = match checkpoint_request {
                        Ok(inner) => inner,
                        Err(panic_payload) => {
                            let panic_msg = if let Some(s) = panic_payload.downcast_ref::<String>()
                            {
                                s.clone()
                            } else if let Some(s) = panic_payload.downcast_ref::<&str>() {
                                s.to_string()
                            } else {
                                "unknown panic".to_string()
                            };
                            tracing::error!(
                                component = "daemon",
                                phase = "checkpoint_side_effect",
                                reason = "panic_in_side_effect",
                                panic_msg = %panic_msg,
                                %family,
                                order,
                                "checkpoint side effect panic"
                            );
                            Err(GitAiError::Generic(format!(
                                "daemon checkpoint panic: {}",
                                panic_msg
                            )))
                        }
                    };
                    let checkpoint_duration_ms = checkpoint_start.elapsed().as_millis();
                    if result.is_ok() {
                        tracing::info!(
                            kind = %checkpoint_kind_str,
                            repo = %repo_wd,
                            duration_ms = checkpoint_duration_ms as u64,
                            "checkpoint done"
                        );
                    } else {
                        tracing::warn!(
                            kind = %checkpoint_kind_str,
                            repo = %repo_wd,
                            duration_ms = checkpoint_duration_ms as u64,
                            "checkpoint failed"
                        );
                    }
                    if result.is_ok() {
                        // Clear pending AI edit state once the PostFileEdit completes.
                        if checkpoint_kind.is_ai()
                            && checkpoint_path_role == PreparedPathRole::Edited
                        {
                            self.clear_pending_ai_edits(family, &checkpoint_file_paths);
                        }
                        let per_file = if !checkpoint_file_paths.is_empty() {
                            compute_watermarks_from_stat(&repo_wd, &checkpoint_file_paths)
                        } else {
                            std::collections::HashMap::new()
                        };
                        let per_worktree = if is_human_checkpoint {
                            let now_ns = std::time::SystemTime::now()
                                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_nanos();
                            std::collections::HashMap::from([(repo_wd.clone(), now_ns)])
                        } else {
                            std::collections::HashMap::new()
                        };
                        if !per_file.is_empty() || !per_worktree.is_empty() {
                            let _ = self
                                .coordinator
                                .update_watermarks_family(
                                    Path::new(&repo_wd),
                                    crate::daemon::domain::WatermarkState {
                                        per_file,
                                        per_worktree,
                                    },
                                )
                                .await;
                        }
                    }
                    // Removed captured_checkpoint_id cleanup - no more captured checkpoints
                    if let Err(error) = &result {
                        let _ = self.record_side_effect_error(family, order, error);
                        tracing::error!(
                            %error,
                            %family,
                            order,
                            "checkpoint side effect failed"
                        );
                    }
                    if should_log_completion {
                        let log_entry = TestCompletionLogEntry {
                            seq: result.as_ref().copied().unwrap_or(0),
                            family_key: family.to_string(),
                            kind: "checkpoint".to_string(),
                            primary_command: Some("checkpoint".to_string()),
                            test_sync_session: None,
                            exit_code: None,
                            sync_tracked: true,
                            status: if result.is_ok() {
                                "ok".to_string()
                            } else {
                                "error".to_string()
                            },
                            error: result.as_ref().err().map(|error| error.to_string()),
                        };
                        if let Err(error) =
                            self.maybe_append_test_completion_log(family, &log_entry)
                        {
                            let _ = self.record_side_effect_error(family, order, &error);
                            tracing::error!(
                                %error,
                                %family,
                                order,
                                "checkpoint completion log write failed"
                            );
                        }
                    }
                    if let Some(respond_to) = respond_to {
                        let _ = respond_to.send(result);
                    }
                }
                FamilySequencerEntry::Canceled => {}
                FamilySequencerEntry::PendingRoot => {}
            }
        }
        let _ = self.end_family_effect(family);

        let _ = progressed;
        Ok(())
    }

    fn rewrite_worktree_key(worktree: &Path) -> String {
        let normalized = worktree_root_for_path(worktree).unwrap_or_else(|| worktree.to_path_buf());
        normalized
            .canonicalize()
            .unwrap_or(normalized)
            .to_string_lossy()
            .to_string()
    }

    fn set_pending_rebase_original_head_for_worktree(
        &self,
        worktree: &Path,
        original_head: String,
        onto: Option<String>,
    ) -> Result<(), GitAiError> {
        let mut map = self
            .pending_rebase_original_head_by_worktree
            .lock()
            .map_err(|_| {
                GitAiError::Generic("pending rebase original-head map lock poisoned".to_string())
            })?;
        map.insert(Self::rewrite_worktree_key(worktree), (original_head, onto));
        Ok(())
    }

    fn clear_pending_rebase_original_head_for_worktree(
        &self,
        worktree: &Path,
    ) -> Result<(), GitAiError> {
        let mut map = self
            .pending_rebase_original_head_by_worktree
            .lock()
            .map_err(|_| {
                GitAiError::Generic("pending rebase original-head map lock poisoned".to_string())
            })?;
        map.remove(&Self::rewrite_worktree_key(worktree));
        Ok(())
    }

    fn take_pending_rebase_original_head_for_worktree(
        &self,
        worktree: &Path,
    ) -> Result<Option<(String, Option<String>)>, GitAiError> {
        let mut map = self
            .pending_rebase_original_head_by_worktree
            .lock()
            .map_err(|_| {
                GitAiError::Generic("pending rebase original-head map lock poisoned".to_string())
            })?;
        Ok(map.remove(&Self::rewrite_worktree_key(worktree)))
    }

    fn set_pending_cherry_pick_sources_for_worktree(
        &self,
        worktree: &Path,
        sources: Vec<String>,
    ) -> Result<(), GitAiError> {
        let mut map = self
            .pending_cherry_pick_sources_by_worktree
            .lock()
            .map_err(|_| {
                GitAiError::Generic("pending cherry-pick sources map lock poisoned".to_string())
            })?;
        let key = Self::rewrite_worktree_key(worktree);
        if sources.is_empty() {
            map.remove(&key);
        } else {
            map.insert(key, sources);
        }
        Ok(())
    }

    fn clear_pending_cherry_pick_sources_for_worktree(
        &self,
        worktree: &Path,
    ) -> Result<(), GitAiError> {
        let mut map = self
            .pending_cherry_pick_sources_by_worktree
            .lock()
            .map_err(|_| {
                GitAiError::Generic("pending cherry-pick sources map lock poisoned".to_string())
            })?;
        map.remove(&Self::rewrite_worktree_key(worktree));
        Ok(())
    }

    fn take_pending_cherry_pick_sources_for_worktree(
        &self,
        worktree: &Path,
    ) -> Result<Vec<String>, GitAiError> {
        let mut map = self
            .pending_cherry_pick_sources_by_worktree
            .lock()
            .map_err(|_| {
                GitAiError::Generic("pending cherry-pick sources map lock poisoned".to_string())
            })?;
        Ok(map
            .remove(&Self::rewrite_worktree_key(worktree))
            .unwrap_or_default())
    }

    fn resolve_heads_for_command(
        cmd: &crate::daemon::domain::NormalizedCommand,
    ) -> (String, String) {
        let reflog_old_head = cmd
            .post_repo
            .as_ref()
            .and_then(|repo| repo.head.as_deref())
            .filter(|head| is_valid_oid(head) && !is_zero_oid(head))
            .and_then(|new_head| {
                cmd.worktree.as_deref().and_then(|worktree| {
                    stable_old_head_from_worktree_head_reflog(worktree, new_head)
                })
            });
        let old = cmd
            .ref_changes
            .iter()
            .find(|change| change.reference == "HEAD")
            .map(|change| change.old.clone())
            .or_else(|| {
                cmd.ref_changes
                    .iter()
                    .find(|change| change.reference.starts_with("refs/heads/"))
                    .map(|change| change.old.clone())
            })
            .or_else(|| {
                cmd.ref_changes
                    .iter()
                    .find(|change| change.reference == "ORIG_HEAD")
                    .map(|change| change.new.clone())
            })
            .or_else(|| {
                cmd.ref_changes
                    .iter()
                    .find(|change| is_non_auxiliary_ref(&change.reference))
                    .map(|change| change.old.clone())
            })
            .or(reflog_old_head)
            .or_else(|| cmd.pre_repo.as_ref().and_then(|repo| repo.head.clone()))
            .unwrap_or_default();
        let new = cmd
            .ref_changes
            .iter()
            .rfind(|change| change.reference == "HEAD")
            .map(|change| change.new.clone())
            .or_else(|| {
                cmd.ref_changes
                    .iter()
                    .rfind(|change| change.reference.starts_with("refs/heads/"))
                    .map(|change| change.new.clone())
            })
            .or_else(|| {
                cmd.ref_changes
                    .iter()
                    .rfind(|change| is_non_auxiliary_ref(&change.reference))
                    .map(|change| change.new.clone())
            })
            .or_else(|| cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()))
            .unwrap_or_default();
        (old, new)
    }

    fn stash_pathspecs_from_command(cmd: &crate::daemon::domain::NormalizedCommand) -> Vec<String> {
        let parsed = parsed_invocation_for_normalized_command(cmd);
        if parsed.command.as_deref() != Some("stash") {
            return Vec::new();
        }

        let mut pathspecs = Vec::new();
        let mut found_separator = false;
        let mut skip_next = false;

        for (i, arg) in parsed.command_args.iter().enumerate() {
            if skip_next {
                skip_next = false;
                continue;
            }
            if arg == "--" {
                found_separator = true;
                continue;
            }
            if found_separator {
                pathspecs.push(arg.clone());
                continue;
            }
            if arg.starts_with('-') {
                if matches!(
                    arg.as_str(),
                    "-m" | "--message" | "--pathspec-from-file" | "--pathspec-file-nul"
                ) {
                    skip_next = true;
                }
                continue;
            }
            if i == 0 && matches!(arg.as_str(), "push" | "save" | "pop" | "apply") {
                continue;
            }
            if i == 1 && arg.starts_with("stash@") {
                continue;
            }
            pathspecs.push(arg.clone());
        }

        tracing::debug!("Extracted stash pathspecs: {:?}", pathspecs);
        pathspecs
    }

    fn stable_rebase_heads_from_worktree(
        repository: &Repository,
        worktree: &Path,
        argv: &[String],
        start_target_hint: Option<&str>,
    ) -> Result<Option<(String, String, String)>, GitAiError> {
        let processed_new_heads = processed_rebase_new_heads(repository)?;
        let mut segment =
            resolve_rebase_segment_for_worktree(worktree, start_target_hint, &processed_new_heads)?;
        let Some(mut segment) = segment.take() else {
            return Ok(None);
        };

        if let Some(branch_ref) = resolve_explicit_rebase_branch_ref(worktree, argv)
            && let Some(original_head) = resolve_reflog_old_oid_for_ref_new_oid_in_worktree(
                worktree,
                &branch_ref,
                &segment.new_head,
            )
            && original_head != segment.new_head
        {
            segment.original_head = original_head;
        }

        Ok(Some((
            segment.original_head,
            segment.new_head,
            segment.onto_head,
        )))
    }

    /// Detects non-fast-forward ref moves and fires handle_rewrite_event.
    fn detect_and_handle_non_ff_rewrites(
        &self,
        cmd: &crate::daemon::domain::NormalizedCommand,
    ) -> Result<(), GitAiError> {
        let worktree = match cmd.worktree.as_ref() {
            Some(w) => w,
            None => return Ok(()),
        };

        let repo = find_repository_in_path(&worktree.to_string_lossy())?;

        // For rebase --skip/--continue that completes successfully, the trace2 data only shows
        // HEAD moving from onto → new_tip (a fast-forward). The real old_tip (original branch tip
        // before rebase started) was stored when the initial rebase failed. Use it here.
        let is_rebase_cmd = cmd.primary_command.as_deref() == Some("rebase");
        let pending_original_head = if is_rebase_cmd {
            self.take_pending_rebase_original_head_for_worktree(worktree)?
        } else {
            None
        };

        // Collect branch ref changes (skip notes, tags, etc.)
        let mut branch_changes: Vec<_> = cmd
            .ref_changes
            .iter()
            .filter(|rc| rc.reference.starts_with("refs/heads/"))
            .filter(|rc| is_valid_oid(&rc.old) && !is_zero_oid(&rc.old))
            .filter(|rc| is_valid_oid(&rc.new) && !is_zero_oid(&rc.new))
            .cloned()
            .collect();

        // If no branch ref changes found, fall back to HEAD changes (common for reset)
        if branch_changes.is_empty() {
            let head_changes: Vec<_> = cmd
                .ref_changes
                .iter()
                .filter(|rc| rc.reference == "HEAD")
                .filter(|rc| is_valid_oid(&rc.old) && !is_zero_oid(&rc.old))
                .filter(|rc| is_valid_oid(&rc.new) && !is_zero_oid(&rc.new))
                .cloned()
                .collect();
            if !head_changes.is_empty() {
                branch_changes = head_changes;
            }
        }

        if branch_changes.is_empty() && pending_original_head.is_none() {
            return Ok(());
        }

        // Collapse multiple changes to same branch: use (first old, last new)
        let mut collapsed: std::collections::HashMap<&str, (&str, &str)> =
            std::collections::HashMap::new();
        for rc in &branch_changes {
            collapsed
                .entry(rc.reference.as_str())
                .and_modify(|(_old, new)| *new = &rc.new)
                .or_insert((&rc.old, &rc.new));
        }

        // Extract "onto" hint from HEAD ref changes for rebases.
        // During a rebase, the first HEAD change target is the onto commit.
        let onto_hint: Option<String> = cmd
            .ref_changes
            .iter()
            .filter(|rc| rc.reference == "HEAD")
            .filter(|rc| is_valid_oid(&rc.new) && !is_zero_oid(&rc.new))
            .map(|rc| rc.new.clone())
            .next();

        // If we have a pending original head from a failed rebase, use it as old_tip
        // with the last HEAD new value as new_tip. This handles rebase --skip/--continue
        // where trace2 only shows the within-command HEAD movement (onto → new_tip).
        if let Some((original_head, stored_onto)) = pending_original_head {
            let new_tip = cmd
                .ref_changes
                .iter()
                .filter(|rc| rc.reference == "HEAD" || rc.reference.starts_with("refs/heads/"))
                .filter(|rc| is_valid_oid(&rc.new) && !is_zero_oid(&rc.new))
                .map(|rc| rc.new.clone())
                .next_back();
            let rebase_onto = stored_onto;
            if let Some(new_tip) = new_tip
                && original_head != new_tip
                && !is_ancestor_commit(&repo, &original_head, &new_tip)
            {
                crate::authorship::rewrite::handle_rewrite_event(
                    &repo,
                    crate::authorship::rewrite::RewriteEvent::NonFastForward {
                        old_tip: original_head,
                        new_tip: new_tip.clone(),
                        onto: rebase_onto.clone(),
                    },
                )?;
                process_conflict_resolution_working_logs(&repo, &new_tip, rebase_onto.as_deref());
            }
            return Ok(());
        }

        for (old_tip, new_tip) in collapsed.values() {
            if *old_tip == *new_tip {
                continue;
            }

            // Fast-forward — not a rewrite
            if is_ancestor_commit(&repo, old_tip, new_tip) {
                continue;
            }

            crate::authorship::rewrite::handle_rewrite_event(
                &repo,
                crate::authorship::rewrite::RewriteEvent::NonFastForward {
                    old_tip: old_tip.to_string(),
                    new_tip: new_tip.to_string(),
                    onto: onto_hint.clone(),
                },
            )?;
        }

        Ok(())
    }

    async fn maybe_apply_side_effects_for_applied_command(
        &self,
        family: Option<&str>,
        applied: &crate::daemon::domain::AppliedCommand,
    ) -> Result<(), GitAiError> {
        // Test-only: allow inducing a panic in the side-effect pipeline to verify
        // that the daemon's catch_unwind recovery keeps the process alive.
        // Uses a file-based flag so the test can remove the file between commands.
        #[cfg(feature = "test-support")]
        if let Ok(path) = std::env::var("GIT_AI_TEST_PANIC_IN_SIDE_EFFECT_FLAG")
            && std::path::Path::new(&path).exists()
        {
            panic!("test-induced panic in side-effect pipeline");
        }

        let cmd = &applied.command;
        let events = &applied.analysis.events;

        let primary = cmd.primary_command.as_deref().unwrap_or("unknown");
        let is_write_op = matches!(
            primary,
            "commit"
                | "rebase"
                | "merge"
                | "cherry-pick"
                | "am"
                | "stash"
                | "reset"
                | "push"
                | "update-ref"
        );
        if is_write_op && cmd.exit_code == 0 {
            let repo_path = cmd
                .worktree
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let post_head = cmd
                .post_repo
                .as_ref()
                .and_then(|r| r.head.clone())
                .unwrap_or_default();
            tracing::info!(
                op = primary,
                repo = %repo_path,
                new_head = %post_head,
                "git write op completed"
            );
        }

        let saw_pull_event = events.iter().any(|event| {
            matches!(
                event,
                crate::daemon::domain::SemanticEvent::PullCompleted { .. }
            )
        });
        let pull_uses_rebase = events.iter().any(|event| {
            matches!(
                event,
                crate::daemon::domain::SemanticEvent::PullCompleted {
                    strategy: crate::daemon::domain::PullStrategy::Rebase
                        | crate::daemon::domain::PullStrategy::RebaseMerges,
                    ..
                }
            )
        });
        if std::env::var("GIT_AI_DEBUG_DAEMON_TRACE")
            .ok()
            .as_deref()
            .is_some_and(|v| v == "1")
        {
            tracing::debug!(
                command = cmd.invoked_command.clone().unwrap_or_default(),
                primary = cmd.primary_command.clone().unwrap_or_default(),
                seq = applied.seq,
                argv = ?cmd.raw_argv,
                invoked_args = ?cmd.invoked_args,
                ref_changes_len = cmd.ref_changes.len(),
                ref_changes = ?cmd.ref_changes,
                events = ?events,
                pre_head = ?cmd.pre_repo.as_ref().and_then(|repo| repo.head.clone()),
                post_head = ?cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()),
                exit_code = cmd.exit_code,
                "side-effect trace"
            );
            tracing::debug!(
                inflight_rebase_original_head = ?cmd.inflight_rebase_original_head,
                "side-effect inflight rebase state"
            );
        }
        let carryover_snapshot = if let Some(snapshot_id) = cmd.carryover_snapshot_id.as_deref() {
            self.take_carryover_snapshot(&cmd.root_sid, snapshot_id)?
        } else {
            None
        };
        let deferred_rewrite_carryover = if let (Some(snapshot), Some(worktree)) =
            (carryover_snapshot.as_ref(), cmd.worktree.as_ref())
        {
            let needs_restore_after_rewrite = cmd.primary_command.as_deref() == Some("rebase")
                || (saw_pull_event && pull_uses_rebase);
            if needs_restore_after_rewrite {
                let (old_head, new_head) = Self::resolve_heads_for_command(cmd);
                if !old_head.is_empty() && !new_head.is_empty() && old_head != new_head {
                    let repo = find_repository_in_path(&worktree.to_string_lossy())?;
                    let tracked_files = tracked_working_log_files(&repo, &old_head)?;
                    if tracked_files.is_empty() {
                        None
                    } else {
                        let carried_va = crate::authorship::virtual_attribution::VirtualAttributions::from_persisted_working_log(
                            repo.clone(),
                            old_head.clone(),
                            Some(repo.git_author_identity().formatted_or_unknown()),
                        )?;
                        Some((new_head, carried_va, snapshot.clone()))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if deferred_rewrite_carryover.is_none()
            && carryover_snapshot.is_none()
            && let Some(worktree) = cmd.worktree.as_ref()
            && (cmd.primary_command.as_deref() == Some("rebase")
                || (saw_pull_event && pull_uses_rebase))
        {
            let (old_head, new_head) = Self::resolve_heads_for_command(cmd);
            if !old_head.is_empty() && !new_head.is_empty() && old_head != new_head {
                let repo = find_repository_in_path(&worktree.to_string_lossy())?;
                let tracked_files = tracked_working_log_files(&repo, &old_head)?;
                if !tracked_files.is_empty() {
                    // No carryover snapshot was captured for the direct pre-command HEAD
                    // (old_head = conflict-time HEAD during the rebase pause).  This can happen
                    // legitimately when the working-log entries at old_head are conflict-resolution
                    // checkpoints written by `git-ai checkpoint` during `rebase --continue`, rather
                    // than pre-rebase uncommitted attribution changes.
                    //
                    // The carryover snapshot capture uses stable_rebase_heads_from_worktree which
                    // returns the original pre-rebase HEAD (not the conflict-time HEAD), and finds
                    // no files there — so no snapshot is stored.  The conflict-resolution attribution
                    // is handled independently by build_note_from_conflict_wl via the rewrite-log
                    // path, which does not require the carryover snapshot.
                    //
                    // If there were genuine pre-rebase uncommitted attribution files, the snapshot
                    // capture would have found them at the original pre-rebase HEAD and stored the
                    // snapshot — in that case carryover_snapshot would be Some and the guard would
                    // not fire.  So reaching here means there are no pre-rebase uncommitted files
                    // to carry over, and the warning is benign.
                    tracing::warn!(
                        command = cmd.primary_command.as_deref().unwrap_or("pull"),
                        "missing captured carryover snapshot for async restore (likely AI conflict-resolution checkpoint; attribution handled via working-log fallback)"
                    );
                }
            }
        }

        // Non-FF rewrite detection: fires for commands that rewrite history via ref moves.
        // Skip for: checkout/switch/branch (no rewriting), cherry-pick (handled separately),
        // and plain commit/amend (CommitCreated/CommitAmended events handle those).
        // Do NOT skip for rebase — the CommitCreated events during rebase are intermediate
        // replayed commits; note transfer happens via non-FF detection on the final ref move.
        // But DO skip for rebase --abort/--skip (those restore state, not rewrite).
        let is_rebase = cmd.primary_command.as_deref() == Some("rebase");
        let is_rebase_abort = is_rebase && cmd.invoked_args.iter().any(|a| a == "--abort");
        let is_completing_rebase = is_rebase && !is_rebase_abort;
        let is_pull_rebase = pull_uses_rebase && cmd.primary_command.as_deref() == Some("pull");
        let skip_non_ff = if is_completing_rebase || is_pull_rebase {
            false
        } else if is_rebase_abort {
            true
        } else {
            events.iter().any(|event| {
                matches!(
                    event,
                    crate::daemon::domain::SemanticEvent::CommitAmended { .. }
                        | crate::daemon::domain::SemanticEvent::CommitCreated { .. }
                        | crate::daemon::domain::SemanticEvent::CherryPickComplete { .. }
                        | crate::daemon::domain::SemanticEvent::Reset { .. }
                )
            }) || matches!(
                cmd.primary_command.as_deref(),
                Some("checkout" | "switch" | "branch" | "stash")
            )
        };
        if !skip_non_ff
            && cmd.exit_code == 0
            && let Err(e) = self.detect_and_handle_non_ff_rewrites(cmd)
        {
            tracing::debug!(
                sid = %cmd.root_sid,
                %e,
                "non-ff rewrite detection failed (non-fatal)"
            );
        }

        if cmd.exit_code != 0 {
            let is_rebase_like = cmd.primary_command.as_deref() == Some("rebase")
                || (cmd.primary_command.as_deref() == Some("pull") && pull_uses_rebase);
            if is_rebase_like {
                let worktree = cmd.worktree.as_ref().ok_or_else(|| {
                    GitAiError::Generic(format!(
                        "rebase side-effect state requires worktree sid={}",
                        cmd.root_sid
                    ))
                })?;
                if cmd.invoked_args.iter().any(|arg| arg == "--abort") {
                    self.clear_pending_rebase_original_head_for_worktree(worktree)?;
                } else if cmd.exit_code != 0 && !rebase_is_control_mode(cmd) {
                    let pending_old_head = strict_rebase_original_head_from_command(cmd, "");
                    if let Some(old_head) = pending_old_head {
                        let rebase_onto = resolve_rebase_onto_for_worktree(worktree);
                        if std::env::var("GIT_AI_DEBUG_DAEMON_TRACE")
                            .ok()
                            .as_deref()
                            .is_some_and(|v| v == "1")
                        {
                            tracing::debug!(
                                ?family,
                                %old_head,
                                ?rebase_onto,
                                "pending rebase original head set"
                            );
                        }
                        self.set_pending_rebase_original_head_for_worktree(
                            worktree,
                            old_head,
                            rebase_onto,
                        )?;
                    }
                }
            }
            if cmd.primary_command.as_deref() == Some("cherry-pick") {
                let worktree = cmd.worktree.as_ref().ok_or_else(|| {
                    GitAiError::Generic(format!(
                        "cherry-pick side-effect state requires worktree sid={}",
                        cmd.root_sid
                    ))
                })?;
                if cmd.invoked_args.iter().any(|arg| arg == "--abort") {
                    self.clear_pending_cherry_pick_sources_for_worktree(worktree)?;
                } else if cmd.exit_code != 0 {
                    let source_refs = cherry_pick_source_refs_from_command(cmd);
                    self.set_pending_cherry_pick_sources_for_worktree(worktree, source_refs)?;
                }
            }
            // Fix #957: `checkout/switch --merge` exits with code 1 when it produces
            // conflict markers but HEAD still moves to the target branch.  We must not
            // return early here — fall through so apply_checkout_switch_working_log_side_effect
            // and recent_checkout_switch_prerequisite_from_command can migrate the working log.
            let is_merge_checkout =
                matches!(cmd.primary_command.as_deref(), Some("checkout" | "switch")) && {
                    let p = parsed_invocation_for_normalized_command(cmd);
                    p.has_command_flag("--merge") || p.has_command_flag("-m")
                };
            // For stash pop/apply/branch with non-zero exit (typically conflict), don't
            // skip processing. The stash may have been partially applied and attribution
            // should still be restored. We cannot rely on `has_stash_conflict_for_repo`
            // because in daemon mode the conflict check runs lazily at sync time -- by
            // which point the user may already have resolved the conflict with `git add`.
            // Instead, always attempt restoration for stash restore operations; if the
            // stash was never applied the restore is a harmless no-op.
            let is_stash_restore = cmd.primary_command.as_deref() == Some("stash")
                && events.iter().any(|event| {
                    matches!(
                        event,
                        crate::daemon::domain::SemanticEvent::StashOperation {
                            kind: crate::daemon::domain::StashOpKind::Pop
                                | crate::daemon::domain::StashOpKind::Apply
                                | crate::daemon::domain::StashOpKind::Branch,
                            ..
                        }
                    )
                });
            if !is_merge_checkout && !is_stash_restore {
                return Ok(());
            }
            if is_stash_restore {
                tracing::debug!(
                    sid = %cmd.root_sid,
                    "stash restore with non-zero exit, continuing to restore attribution"
                );
            }
        }

        if let Some(worktree) = cmd.worktree.as_ref() {
            let worktree = worktree.to_string_lossy().to_string();
            for event in events {
                match event {
                    crate::daemon::domain::SemanticEvent::CloneCompleted { .. } => {
                        if let Err(e) = apply_clone_notes_sync_side_effect(&worktree) {
                            tracing::debug!(
                                %e,
                                %worktree,
                                "clone notes side effect failed"
                            );
                        }
                    }
                    crate::daemon::domain::SemanticEvent::PullCompleted { .. } => {
                        let _ = apply_pull_notes_sync_side_effect(
                            &worktree,
                            cmd.invoked_command.as_deref(),
                            &cmd.invoked_args,
                        );
                    }
                    crate::daemon::domain::SemanticEvent::PushCompleted { .. } => {
                        let _ = apply_push_side_effect(
                            &worktree,
                            cmd.invoked_command.as_deref(),
                            &cmd.invoked_args,
                        );
                    }
                    crate::daemon::domain::SemanticEvent::CherryPickComplete {
                        original_head,
                        new_head,
                    } => {
                        if !new_head.is_empty() {
                            let repo = find_repository_in_path(&worktree)?;
                            let mut sources =
                                crate::authorship::rewrite_cherry_pick::expand_cherry_pick_sources(
                                    &repo,
                                    &cmd.invoked_args,
                                );
                            // For --continue, args don't contain source refs — use stored state
                            if sources.is_empty() {
                                sources = self.take_pending_cherry_pick_sources_for_worktree(
                                    worktree.as_ref(),
                                )?;
                            } else {
                                self.clear_pending_cherry_pick_sources_for_worktree(
                                    worktree.as_ref(),
                                )?;
                            }
                            let new_commits =
                                crate::authorship::rewrite_cherry_pick::new_commits_since(
                                    &repo,
                                    original_head,
                                );
                            let pairs =
                                crate::authorship::rewrite_cherry_pick::match_cherry_pick_pairs(
                                    &repo,
                                    &sources,
                                    &new_commits,
                                );
                            if !pairs.is_empty() {
                                let (src, dst): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
                                let _ = crate::authorship::rewrite::handle_rewrite_event(
                                    &repo,
                                    crate::authorship::rewrite::RewriteEvent::CherryPickComplete {
                                        sources: src,
                                        new_commits: dst,
                                    },
                                );
                            }
                        }
                    }
                    crate::daemon::domain::SemanticEvent::StashOperation { kind, head } => {
                        let repo = find_repository_in_path(&worktree)?;
                        match kind {
                            crate::daemon::domain::StashOpKind::Push
                            | crate::daemon::domain::StashOpKind::Unknown => {
                                let resolved_stash =
                                    cmd.stash_target_oid.as_deref().or_else(|| {
                                        cmd.ref_changes
                                        .iter()
                                        .find(|rc| rc.reference == "refs/stash")
                                        .map(|rc| rc.new.as_str())
                                        .filter(|s| {
                                            !s.is_empty()
                                                && *s != "0000000000000000000000000000000000000000"
                                        })
                                    });
                                if let Some(stash_sha) = resolved_stash {
                                    let push_head = repo
                                        .find_commit(stash_sha.to_string())
                                        .ok()
                                        .and_then(|c| c.parent(0).ok())
                                        .map(|p| p.id().to_string())
                                        .or_else(|| head.clone());
                                    if let Some(head_sha) = push_head.as_deref() {
                                        let pathspecs = Self::stash_pathspecs_from_command(cmd);
                                        let _ =
                                            crate::authorship::rewrite_stash::handle_stash_create(
                                                &repo, stash_sha, head_sha, pathspecs,
                                            );
                                    }
                                }
                            }
                            crate::daemon::domain::StashOpKind::Pop => {
                                if let Some(stash_sha) = resolve_stash_sha(cmd) {
                                    let _ =
                                        crate::authorship::rewrite_stash::handle_stash_pop_or_apply_with_head(
                                            &repo, stash_sha, true, head.as_deref(),
                                        );
                                }
                            }
                            crate::daemon::domain::StashOpKind::Apply
                            | crate::daemon::domain::StashOpKind::Branch => {
                                if let Some(stash_sha) = resolve_stash_sha(cmd) {
                                    let effective_head = if matches!(
                                        kind,
                                        crate::daemon::domain::StashOpKind::Branch
                                    ) {
                                        repo.find_commit(stash_sha.to_string())
                                            .ok()
                                            .and_then(|c| c.parent(0).ok())
                                            .map(|p| p.id().to_string())
                                    } else {
                                        None
                                    };
                                    let target_head = effective_head.as_deref().or(head.as_deref());
                                    let _ =
                                        crate::authorship::rewrite_stash::handle_stash_pop_or_apply_with_head(
                                            &repo, stash_sha, false, target_head,
                                        );
                                }
                            }
                            crate::daemon::domain::StashOpKind::Drop => {
                                if let Some(stash_sha) = resolve_stash_sha(cmd) {
                                    let _ = crate::authorship::rewrite_stash::handle_stash_drop(
                                        &repo, stash_sha,
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                    crate::daemon::domain::SemanticEvent::CommitCreated { base, new_head } => {
                        if !new_head.is_empty() {
                            let repo = find_repository_in_path(&worktree)?;
                            let author = repo.git_author_identity().formatted_or_unknown();
                            let base_opt = base.clone().filter(|b| !b.is_empty() && b != "initial");
                            let parent_sha = base_opt.as_deref().unwrap_or("initial");

                            let _ = sync_pre_commit_checkpoint(
                                &repo,
                                parent_sha,
                                carryover_snapshot.as_ref(),
                            );
                            if let Err(e) =
                                crate::authorship::post_commit::post_commit_with_final_state(
                                    &repo,
                                    base_opt,
                                    new_head.clone(),
                                    author,
                                    true,
                                    carryover_snapshot.as_ref(),
                                )
                            {
                                tracing::debug!(
                                    %e,
                                    %worktree,
                                    "commit post-commit side effect failed"
                                );
                            }
                            // Squash merge: if this commit was created after merge --squash,
                            // transfer authorship notes from the source branch commits.
                            if let Some(source_head) = cmd.merge_squash_source_head.as_deref()
                                && !source_head.is_empty()
                            {
                                let onto = base.clone().unwrap_or_default();
                                if !onto.is_empty() {
                                    let _ = crate::authorship::rewrite::handle_rewrite_event(
                                        &repo,
                                        crate::authorship::rewrite::RewriteEvent::SquashMerge {
                                            source_head: source_head.to_string(),
                                            squash_commit: new_head.clone(),
                                            onto,
                                        },
                                    );
                                }
                            }
                        }
                    }
                    crate::daemon::domain::SemanticEvent::CommitAmended { old_head, new_head } => {
                        if !old_head.is_empty()
                            && !new_head.is_empty()
                            && old_head != new_head
                            && is_valid_oid(old_head)
                            && !is_zero_oid(old_head)
                            && is_valid_oid(new_head)
                            && !is_zero_oid(new_head)
                        {
                            let repo = find_repository_in_path(&worktree)?;
                            let author = repo.git_author_identity().formatted_or_unknown();
                            let committed_snapshot =
                                committed_file_snapshot_for_amend(&repo, old_head, new_head);
                            // Build effective snapshot: start with carryover (captures
                            // working directory state including unstaged content), but
                            // override per-file with committed content when the working
                            // log's latest checkpoint blob matches the committed content.
                            // This corrects for stale carryover snapshots that may have
                            // been read after subsequent filesystem writes raced with
                            // async daemon processing.
                            let effective_snapshot =
                                match (&carryover_snapshot, &committed_snapshot) {
                                    (Some(carryover), Some(committed)) => {
                                        let working_log =
                                            repo.storage.working_log_for_base_commit(old_head).ok();
                                        let mut merged = carryover.clone();
                                        for (file, committed_content) in committed {
                                            let latest_matches_committed = working_log
                                                .as_ref()
                                                .and_then(|wl| {
                                                    wl.latest_checkpoint_file_content(file)
                                                })
                                                .is_some_and(|latest| latest == *committed_content);
                                            if latest_matches_committed {
                                                merged.insert(
                                                    file.clone(),
                                                    committed_content.clone(),
                                                );
                                            }
                                        }
                                        Some(merged)
                                    }
                                    (Some(c), None) => Some(c.clone()),
                                    (None, Some(c)) => Some(c.clone()),
                                    (None, None) => None,
                                };
                            let effective_snapshot = effective_snapshot.as_ref();
                            if let Err(e) =
                                crate::authorship::post_commit::post_commit_amend_with_final_state(
                                    &repo,
                                    old_head,
                                    new_head,
                                    author,
                                    effective_snapshot,
                                )
                            {
                                tracing::debug!(
                                    %e,
                                    %worktree,
                                    "commit amend side effect failed"
                                );
                            }
                        }
                    }
                    crate::daemon::domain::SemanticEvent::Reset {
                        kind,
                        old_head,
                        new_head,
                    } => {
                        if !old_head.is_empty() && !new_head.is_empty() && old_head != new_head {
                            let repo = find_repository_in_path(&worktree)?;
                            match kind {
                                crate::daemon::domain::ResetKind::Hard => {
                                    let _ =
                                        repo.storage.delete_working_log_for_base_commit(old_head);
                                }
                                _ => {
                                    if is_ancestor_commit(&repo, new_head, old_head) {
                                        let _ = crate::authorship::rewrite_reset::reconstruct_working_log_after_backward_reset(
                                            &repo, old_head, new_head,
                                        );
                                    } else if !is_ancestor_commit(&repo, old_head, new_head) {
                                        let _ = crate::authorship::rewrite::handle_rewrite_event(
                                            &repo,
                                            crate::authorship::rewrite::RewriteEvent::NonFastForward {
                                                old_tip: old_head.to_string(),
                                                new_tip: new_head.to_string(),
                                                onto: None,
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if let Some((new_head, carried_va, snapshot)) = deferred_rewrite_carryover
            && let Some(worktree) = cmd.worktree.as_ref()
        {
            let repo = find_repository_in_path(&worktree.to_string_lossy())?;
            restore_virtual_attribution_carryover(&repo, &new_head, carried_va, snapshot)?;
        }

        if matches!(cmd.primary_command.as_deref(), Some("checkout" | "switch")) {
            if let Some(prerequisite) =
                recent_checkout_switch_prerequisite_from_command(cmd, carryover_snapshot.as_ref())
            {
                let family = family.map(std::borrow::ToOwned::to_owned).or_else(|| {
                    cmd.worktree.as_ref().and_then(|worktree| {
                        find_repository_in_path(&worktree.to_string_lossy())
                            .ok()
                            .map(|repo| family_key_for_repository(&repo))
                    })
                });
                if let Some(family) = family {
                    self.record_recent_replay_prerequisite(&family, prerequisite)?;
                }
            }
            apply_checkout_switch_working_log_side_effect(cmd, carryover_snapshot.as_ref())?;
        }

        if saw_pull_event
            && !pull_uses_rebase
            && let Some(worktree) = cmd.worktree.as_ref()
        {
            let (old_head, new_head) = Self::resolve_heads_for_command(cmd);
            if !old_head.is_empty()
                && !new_head.is_empty()
                && old_head != new_head
                && let Ok(repo) = find_repository_in_path(&worktree.to_string_lossy())
                && repo_is_ancestor(&repo, &old_head, &new_head)
            {
                apply_pull_fast_forward_working_log_side_effect(
                    &worktree.to_string_lossy(),
                    &old_head,
                    &new_head,
                )?;
            }
        }

        // Handle update-ref: migrate working logs and authorship notes when the ref
        // update affects the currently checked-out branch.
        if primary == "update-ref"
            && let Some(worktree) = cmd.worktree.as_ref()
        {
            let current_branch = cmd.pre_repo.as_ref().and_then(|r| r.branch.clone());
            for event in events {
                if let crate::daemon::domain::SemanticEvent::RefUpdated {
                    reference,
                    old,
                    new,
                } = event
                {
                    if !reference.starts_with("refs/heads/")
                        || !is_valid_oid(old)
                        || is_zero_oid(old)
                        || !is_valid_oid(new)
                        || is_zero_oid(new)
                        || old == new
                    {
                        continue;
                    }
                    if let Ok(repo) = find_repository_in_path(&worktree.to_string_lossy()) {
                        if repo_is_ancestor(&repo, old, new) {
                            let affects_checked_out_branch =
                                current_branch.as_deref().is_some_and(|branch| {
                                    reference == &format!("refs/heads/{}", branch)
                                        || reference == branch
                                });
                            if affects_checked_out_branch {
                                let _ = repo.storage.rename_working_log(old, new);
                            }
                        } else {
                            let _ = crate::authorship::rewrite::handle_rewrite_event(
                                &repo,
                                crate::authorship::rewrite::RewriteEvent::NonFastForward {
                                    old_tip: old.to_string(),
                                    new_tip: new.to_string(),
                                    onto: None,
                                },
                            );
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn apply_trace_payload_to_state(
        &self,
        payload: Value,
    ) -> Result<TracePayloadApplyOutcome, GitAiError> {
        self.maybe_append_pending_root_from_trace_payload(&payload)?;
        let payload_root_sid = Self::trace_payload_root_sid(&payload);
        let event = payload
            .get("event")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let emitted = {
            let mut normalizer = self.normalizer.lock().await;
            normalizer.ingest_payload(&payload)?
        };
        let Some(command) = emitted else {
            if is_terminal_root_trace_event(
                &event,
                payload
                    .get("sid")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                payload_root_sid.as_deref().unwrap_or_default(),
            ) && let Some(root_sid) = payload_root_sid.as_deref()
                && let Some(family) = self
                    .replace_pending_root_entry(root_sid, FamilySequencerEntry::Canceled)
                    .await?
            {
                self.clear_trace_root_tracking(root_sid)?;
                let _ = family;
                return Ok(TracePayloadApplyOutcome::QueuedFamily);
            }
            return Ok(TracePayloadApplyOutcome::None);
        };
        let root_sid = command.root_sid.clone();

        let outcome = if let Some(family) = self
            .replace_pending_root_entry(
                &root_sid,
                FamilySequencerEntry::ReadyCommand(Box::new(command.clone())),
            )
            .await?
        {
            let _ = family;
            TracePayloadApplyOutcome::QueuedFamily
        } else {
            match self.coordinator.route_command(command).await {
                Ok(applied) => TracePayloadApplyOutcome::Applied(Box::new(applied)),
                Err(error) => {
                    let _ = self.clear_trace_root_tracking(&root_sid);
                    let _ = self.discard_carryover_snapshots_for_root(&root_sid);
                    return Err(error);
                }
            }
        };
        self.clear_trace_root_tracking(&root_sid)?;
        Ok(outcome)
    }

    async fn ingest_trace_payload_fast(self: Arc<Self>, payload: Value) -> Result<(), GitAiError> {
        if !is_trace_payload(&payload) {
            return Ok(());
        }
        match self.apply_trace_payload_to_state(payload).await? {
            TracePayloadApplyOutcome::None | TracePayloadApplyOutcome::QueuedFamily => {}
            TracePayloadApplyOutcome::Applied(mut applied) => {
                if let Some(family) = applied.command.family_key.as_ref().map(|key| key.0.clone()) {
                    self.begin_family_effect(&family)?;
                    if applied.command.wrapper_invocation_id.is_some() {
                        self.apply_wrapper_state_overlay(&mut applied.command).await;
                    }
                    let result = self
                        .maybe_apply_side_effects_for_applied_command(Some(&family), &applied)
                        .await;
                    let _ = self.end_family_effect(&family);
                    if let Err(error) = result {
                        let _ = self.record_side_effect_error(&family, applied.seq, &error);
                        tracing::error!(
                            %error,
                            %family,
                            seq = applied.seq,
                            "async side-effect error"
                        );
                    } else if let Err(error) =
                        self.append_command_completion_log(&family, &applied, &Ok(()), applied.seq)
                    {
                        let _ = self.record_side_effect_error(&family, applied.seq, &error);
                        tracing::error!(
                            %error,
                            %family,
                            seq = applied.seq,
                            "async completion log write failed"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    async fn ingest_checkpoint_payload(
        &self,
        request: CheckpointRequest,
    ) -> Result<ControlResponse, GitAiError> {
        if request.files.is_empty() {
            return Ok(ControlResponse::ok(None, None));
        }

        let repo_work_dir = request.files[0].repo_work_dir.clone();
        let family = self.backend.resolve_family(&repo_work_dir)?;

        self.append_checkpoint_to_family_sequencer(&family.0, request, None)
            .await?;
        Ok(ControlResponse::ok(None, None))
    }

    async fn watermarks_for_family(
        &self,
        repo_working_dir: String,
    ) -> Result<crate::daemon::domain::WatermarkState, GitAiError> {
        self.coordinator
            .watermarks_family(Path::new(&repo_working_dir))
            .await
    }

    async fn status_for_family(
        &self,
        repo_working_dir: String,
    ) -> Result<FamilyStatus, GitAiError> {
        let family = self.backend.resolve_family(Path::new(&repo_working_dir))?;
        let status = self
            .coordinator
            .status_family(Path::new(&repo_working_dir))
            .await?;
        let latest_seq = status.applied_seq;
        let family_key = family.0;
        Ok(FamilyStatus {
            family_key: family_key.clone(),
            latest_seq,
            last_error: status
                .last_error
                .or_else(|| self.latest_side_effect_error(&family_key).ok().flatten()),
        })
    }

    async fn handle_control_request(&self, request: ControlRequest) -> ControlResponse {
        let result = match request {
            ControlRequest::CheckpointRun { request } => {
                if let Some(worker) = &self.transcript_worker
                    && let Some(transcript_source) = &request.transcript_source
                {
                    let session_id = transcript_source.session_id.clone();
                    let tool = request
                        .agent_id
                        .as_ref()
                        .map(|aid| aid.tool.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    let trace_id = request.trace_id.clone();
                    let tool_use_id = request.metadata.get("tool_use_id").cloned();

                    let repo_work_dir = request.files.first().map(|f| f.repo_work_dir.clone());

                    if let Some(db) = &self.transcripts_db
                        && let Err(e) = Self::ensure_session_exists(
                            db,
                            &session_id,
                            &tool,
                            transcript_source,
                            repo_work_dir.as_deref(),
                        )
                    {
                        tracing::warn!(session_id = %session_id, error = %e, "failed to ensure session exists");
                    }

                    worker.notify_checkpoint(
                        session_id,
                        tool,
                        trace_id,
                        tool_use_id,
                        transcript_source.path.clone(),
                        repo_work_dir,
                    );
                }

                self.ingest_checkpoint_payload(*request).await
            }
            ControlRequest::StatusFamily { repo_working_dir } => self
                .status_for_family(repo_working_dir)
                .await
                .and_then(|status| {
                    serde_json::to_value(status)
                        .map(|v| ControlResponse::ok(None, Some(v)))
                        .map_err(GitAiError::from)
                }),
            ControlRequest::SnapshotWatermarks { repo_working_dir } => self
                .watermarks_for_family(repo_working_dir.clone())
                .await
                .and_then(|ws| {
                    let worktree_wm = ws.per_worktree.get(&repo_working_dir).copied();
                    serde_json::to_value(json!({
                        "watermarks": ws.per_file,
                        "worktree_watermark": worktree_wm,
                    }))
                    .map(|v| ControlResponse::ok(None, Some(v)))
                    .map_err(GitAiError::from)
                }),
            ControlRequest::SubmitTelemetry { envelopes } => {
                if let Some(worker) = &self.telemetry_worker {
                    worker.submit_telemetry(envelopes).await;
                }
                Ok(ControlResponse::ok(None, None))
            }
            ControlRequest::SubmitCas { records } => {
                if let Some(worker) = &self.telemetry_worker {
                    worker.submit_cas(records).await;
                }
                Ok(ControlResponse::ok(None, None))
            }
            ControlRequest::FlushNotes => {
                // Trigger an immediate notes flush in a blocking task.
                // Fire-and-forget: the periodic flush loop is the safety net.
                tokio::task::spawn_blocking(|| {
                    crate::daemon::telemetry_worker::flush_notes();
                });
                Ok(ControlResponse::ok(None, None))
            }
            ControlRequest::WrapperPreState {
                invocation_id,
                repo_context,
                ..
            } => {
                self.store_wrapper_state(&invocation_id, Some(repo_context), None);
                Ok(ControlResponse::ok(None, None))
            }
            ControlRequest::WrapperPostState {
                invocation_id,
                repo_context,
                ..
            } => {
                self.store_wrapper_state(&invocation_id, None, Some(repo_context));
                Ok(ControlResponse::ok(None, None))
            }
            ControlRequest::BashSessionStart {
                repo_work_dir,
                session_id,
                tool_use_id,
                agent_id,
                metadata,
                stat_snapshot,
            } => {
                let mut state = self.bash_sessions.lock().unwrap();
                state.start_session(
                    session_id,
                    tool_use_id,
                    repo_work_dir,
                    agent_id,
                    metadata,
                    *stat_snapshot,
                );
                Ok(ControlResponse::ok(None, None))
            }
            ControlRequest::BashSessionEnd {
                session_id,
                tool_use_id,
            } => {
                let mut state = self.bash_sessions.lock().unwrap();
                state.end_session(&session_id, &tool_use_id);
                Ok(ControlResponse::ok(None, None))
            }
            ControlRequest::BashSessionQuery { repo_work_dir } => {
                let state = self.bash_sessions.lock().unwrap();
                let response = match state.query_active_for_repo(&repo_work_dir) {
                    Some((key, session)) => {
                        let data = serde_json::to_value(BashSessionQueryResponse {
                            active: true,
                            agent_id: Some(session.agent_id.clone()),
                            session_id: Some(key.0.clone()),
                            tool_use_id: Some(key.1.clone()),
                            metadata: Some(session.metadata.clone()),
                        })
                        .ok();
                        ControlResponse::ok(None, data)
                    }
                    None => {
                        let data = serde_json::to_value(BashSessionQueryResponse {
                            active: false,
                            agent_id: None,
                            session_id: None,
                            tool_use_id: None,
                            metadata: None,
                        })
                        .ok();
                        ControlResponse::ok(None, data)
                    }
                };
                Ok(response)
            }
            ControlRequest::BashSnapshotQuery {
                session_id,
                tool_use_id,
            } => {
                let state = self.bash_sessions.lock().unwrap();
                let response = match state.get_snapshot(&session_id, &tool_use_id) {
                    Some(snapshot) => {
                        let data = serde_json::to_value(BashSnapshotQueryResponse {
                            found: true,
                            stat_snapshot: Some(snapshot.clone()),
                        })
                        .ok();
                        ControlResponse::ok(None, data)
                    }
                    None => {
                        let data = serde_json::to_value(BashSnapshotQueryResponse {
                            found: false,
                            stat_snapshot: None,
                        })
                        .ok();
                        ControlResponse::ok(None, data)
                    }
                };
                Ok(response)
            }
            ControlRequest::Shutdown => Ok(ControlResponse::ok(None, None)),
        };

        match result {
            Ok(response) => response,
            Err(error) => ControlResponse::err(error.to_string()),
        }
    }

    fn ensure_session_exists(
        db: &crate::transcripts::db::TranscriptsDatabase,
        session_id: &str,
        tool: &str,
        transcript_source: &crate::commands::checkpoint_agent::presets::TranscriptSource,
        repo_work_dir: Option<&std::path::Path>,
    ) -> Result<(), String> {
        // Check if session exists
        if db
            .get_session(session_id)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Ok(());
        }

        // Create new session record
        let now = chrono::Utc::now().timestamp();

        let watermark_type = transcript_source.format.watermark_type();

        let initial_watermark = match watermark_type {
            crate::transcripts::watermark::WatermarkType::ByteOffset => {
                Box::new(crate::transcripts::watermark::ByteOffsetWatermark::new(0))
                    as Box<dyn crate::transcripts::watermark::WatermarkStrategy>
            }
            crate::transcripts::watermark::WatermarkType::RecordIndex => {
                Box::new(crate::transcripts::watermark::RecordIndexWatermark::new(0))
                    as Box<dyn crate::transcripts::watermark::WatermarkStrategy>
            }
            crate::transcripts::watermark::WatermarkType::Timestamp => {
                Box::new(crate::transcripts::watermark::TimestampWatermark::new(
                    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
                )) as Box<dyn crate::transcripts::watermark::WatermarkStrategy>
            }
            crate::transcripts::watermark::WatermarkType::Hybrid => Box::new(
                crate::transcripts::watermark::HybridWatermark::new(0, 0, None),
            )
                as Box<dyn crate::transcripts::watermark::WatermarkStrategy>,
        };

        let record = crate::transcripts::db::SessionRecord {
            session_id: session_id.to_string(),
            tool: tool.to_string(),
            transcript_path: transcript_source.path.display().to_string(),
            transcript_format: format!("{:?}", transcript_source.format),
            watermark_type: format!("{:?}", watermark_type),
            watermark_value: initial_watermark.serialize(),
            external_session_id: transcript_source.external_session_id.clone(),
            external_parent_session_id: transcript_source.external_parent_session_id.clone(),
            first_seen_at: now,
            last_processed_at: 0,
            last_known_size: 0,
            last_modified: None,
            processing_errors: 0,
            last_error: None,
            repo_work_dir: repo_work_dir.map(|p| p.display().to_string()),
        };

        db.insert_session(&record).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn store_wrapper_state(
        &self,
        invocation_id: &str,
        pre_repo: Option<RepoContext>,
        post_repo: Option<RepoContext>,
    ) {
        let mut states = self
            .wrapper_states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = states
            .entry(invocation_id.to_string())
            .or_insert_with(|| WrapperStateEntry {
                pre_repo: None,
                post_repo: None,
                received_at_ns: now_unix_nanos(),
            });
        if let Some(pre) = pre_repo {
            entry.pre_repo = Some(pre);
        }
        if let Some(post) = post_repo {
            entry.post_repo = Some(post);
        }
        entry.received_at_ns = now_unix_nanos();
        drop(states);
        self.wrapper_state_notify.notify_waiters();
    }

    async fn apply_wrapper_state_overlay(
        &self,
        command: &mut crate::daemon::domain::NormalizedCommand,
    ) {
        let Some(invocation_id) = command.wrapper_invocation_id.as_ref() else {
            return;
        };
        let invocation_id = invocation_id.clone();
        let timeout = self.wrapper_state_wait_timeout();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            // Register interest in notifications BEFORE checking state.
            // This prevents a race where notify_waiters() fires between
            // our check and our await, causing a lost wakeup.
            let notified = self.wrapper_state_notify.notified();

            let (has_pre, has_post) = {
                let states = self
                    .wrapper_states
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match states.get(&invocation_id) {
                    Some(entry) => (entry.pre_repo.is_some(), entry.post_repo.is_some()),
                    None => (false, false),
                }
            };

            if has_pre && has_post {
                let mut states = self
                    .wrapper_states
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(entry) = states.remove(&invocation_id) {
                    if let Some(pre) = entry.pre_repo {
                        command.pre_repo = Some(pre);
                    }
                    if let Some(post) = entry.post_repo {
                        command.post_repo = Some(post);
                    }
                }
                return;
            }

            if tokio::time::Instant::now() >= deadline {
                eprintln!(
                    "git-ai: wrapper state timeout for invocation {} (pre={}, post={}), using internal state",
                    invocation_id, has_pre, has_post
                );
                let mut states = self
                    .wrapper_states
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                states.remove(&invocation_id);
                return;
            }

            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let _ = tokio::time::timeout(remaining, notified).await;
        }
    }

    fn wrapper_state_wait_timeout(&self) -> Duration {
        let is_test = std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
            || std::env::var_os("GITAI_TEST_DB_PATH").is_some();
        if is_test {
            Duration::from_secs(20)
        } else {
            Duration::from_millis(750)
        }
    }
}

fn control_listener_loop_actor(
    control_socket_path: PathBuf,
    coordinator: Arc<ActorDaemonCoordinator>,
    runtime_handle: tokio::runtime::Handle,
) -> Result<(), GitAiError> {
    #[cfg(not(windows))]
    {
        remove_socket_if_exists(&control_socket_path)?;
        let listener = ListenerOptions::new()
            .name(local_socket_name(&control_socket_path)?)
            .create_sync()
            .map_err(|e| GitAiError::Generic(format!("failed binding control socket: {}", e)))?;
        set_socket_owner_only(&control_socket_path)?;
        for stream in listener.incoming() {
            if coordinator.is_shutting_down() {
                break;
            }
            let Ok(stream) = stream else {
                continue;
            };
            let coord = coordinator.clone();
            let handle = runtime_handle.clone();
            if std::thread::Builder::new()
                .spawn(move || {
                    if let Err(e) = handle_control_connection_actor(stream, coord, handle) {
                        tracing::debug!(%e, "control connection error");
                    }
                })
                .is_err()
            {
                tracing::error!("control listener: failed to spawn handler thread");
                break;
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    {
        let mut workers = Vec::new();
        let first_connecting = windows_pipe_connecting_server(&control_socket_path, true)?;
        {
            let path = control_socket_path.clone();
            let coord = coordinator.clone();
            let handle = runtime_handle.clone();
            workers.push(std::thread::spawn(move || {
                let result =
                    windows_control_pipe_worker_loop(path, first_connecting, coord.clone(), handle);
                if let Err(error) = &result {
                    tracing::error!(%error, "control worker error");
                    coord.request_shutdown();
                }
                result
            }));
        }
        for _ in 1..WINDOWS_CONTROL_PIPE_WORKERS {
            let path = control_socket_path.clone();
            let coord = coordinator.clone();
            let handle = runtime_handle.clone();
            let connecting = windows_pipe_connecting_server(&path, false)?;
            workers.push(std::thread::spawn(move || {
                let result =
                    windows_control_pipe_worker_loop(path, connecting, coord.clone(), handle);
                if let Err(error) = &result {
                    tracing::error!(%error, "control worker error");
                    coord.request_shutdown();
                }
                result
            }));
        }

        while !coordinator.is_shutting_down() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        wake_windows_pipe_workers(&control_socket_path, WINDOWS_CONTROL_PIPE_WORKERS);

        for worker in workers {
            let result = worker
                .join()
                .map_err(|_| GitAiError::Generic("daemon control worker panicked".to_string()))?;
            result?;
        }

        Ok(())
    }
}

#[cfg(windows)]
fn windows_pipe_connecting_server(
    pipe_path: &Path,
    first_instance: bool,
) -> Result<WindowsConnectingServer, GitAiError> {
    let mut options = WindowsPipeOptions::new(pipe_path.as_os_str());
    options
        .first(first_instance)
        .open_mode(WindowsPipeOpenMode::Duplex);
    options.single().map_err(|e| {
        GitAiError::Generic(format!(
            "failed binding windows daemon pipe {}: {}",
            pipe_path.display(),
            e
        ))
    })
}

#[cfg(windows)]
fn wake_windows_pipe_workers(pipe_path: &Path, worker_count: usize) {
    for _ in 0..worker_count {
        let _ = WindowsPipeClient::connect_ms(pipe_path.as_os_str(), 100);
    }
}

#[cfg(windows)]
fn windows_control_pipe_worker_loop(
    control_socket_path: PathBuf,
    mut connecting: WindowsConnectingServer,
    coordinator: Arc<ActorDaemonCoordinator>,
    runtime_handle: tokio::runtime::Handle,
) -> Result<(), GitAiError> {
    loop {
        let mut server = connecting.wait().map_err(|e| {
            GitAiError::Generic(format!(
                "failed accepting control pipe {}: {}",
                control_socket_path.display(),
                e
            ))
        })?;

        if coordinator.is_shutting_down() {
            let _ = server.disconnect();
            break;
        }

        {
            let mut reader = BufReader::new(&mut server);
            if let Err(e) = handle_control_connection_actor_reader(
                &mut reader,
                coordinator.clone(),
                runtime_handle.clone(),
            ) {
                tracing::debug!(%e, "control connection error");
            }
        }

        connecting = server.disconnect().map_err(|e| {
            GitAiError::Generic(format!(
                "failed recycling control pipe {}: {}",
                control_socket_path.display(),
                e
            ))
        })?;
    }

    Ok(())
}

#[cfg(not(windows))]
fn handle_control_connection_actor(
    stream: LocalSocketStream,
    coordinator: Arc<ActorDaemonCoordinator>,
    runtime_handle: tokio::runtime::Handle,
) -> Result<(), GitAiError> {
    let mut reader = BufReader::new(stream);
    handle_control_connection_actor_reader(&mut reader, coordinator, runtime_handle)
}

fn handle_control_connection_actor_reader<R: Read + Write>(
    reader: &mut BufReader<R>,
    coordinator: Arc<ActorDaemonCoordinator>,
    runtime_handle: tokio::runtime::Handle,
) -> Result<(), GitAiError> {
    while let Some(line) = read_json_line(reader)? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<ControlRequest>(trimmed);
        let mut shutdown_after_response = false;
        let response = match parsed {
            Ok(req) => {
                shutdown_after_response = matches!(req, ControlRequest::Shutdown);
                runtime_handle.block_on(async { coordinator.handle_control_request(req).await })
            }
            Err(e) => ControlResponse::err(format!("invalid control request: {}", e)),
        };
        let raw = serde_json::to_string(&response)?;
        reader.get_mut().write_all(raw.as_bytes())?;
        reader.get_mut().write_all(b"\n")?;
        reader.get_mut().flush()?;
        if shutdown_after_response {
            coordinator.request_stop();
        }
    }
    Ok(())
}

fn trace_listener_loop_actor(
    trace_socket_path: PathBuf,
    coordinator: Arc<ActorDaemonCoordinator>,
) -> Result<(), GitAiError> {
    #[cfg(not(windows))]
    {
        remove_socket_if_exists(&trace_socket_path)?;
        let listener = ListenerOptions::new()
            .name(local_socket_name(&trace_socket_path)?)
            .create_sync()
            .map_err(|e| GitAiError::Generic(format!("failed binding trace socket: {}", e)))?;
        set_socket_owner_only(&trace_socket_path)?;
        for stream in listener.incoming() {
            if coordinator.is_shutting_down() {
                break;
            }
            let Ok(stream) = stream else {
                continue;
            };
            let coord = coordinator.clone();
            if std::thread::Builder::new()
                .spawn(move || {
                    if let Err(e) = handle_trace_connection_actor(stream, coord) {
                        tracing::debug!(%e, "trace connection error");
                    }
                })
                .is_err()
            {
                tracing::error!("trace listener: failed to spawn handler thread");
                break;
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    {
        let mut workers = Vec::new();
        let first_connecting = windows_pipe_connecting_server(&trace_socket_path, true)?;
        {
            let path = trace_socket_path.clone();
            let coord = coordinator.clone();
            workers.push(std::thread::spawn(move || {
                let result = windows_trace_pipe_worker_loop(path, first_connecting, coord.clone());
                if let Err(error) = &result {
                    tracing::error!(%error, "trace worker error");
                    coord.request_shutdown();
                }
                result
            }));
        }
        for _ in 1..WINDOWS_TRACE_PIPE_WORKERS {
            let path = trace_socket_path.clone();
            let coord = coordinator.clone();
            let connecting = windows_pipe_connecting_server(&path, false)?;
            workers.push(std::thread::spawn(move || {
                let result = windows_trace_pipe_worker_loop(path, connecting, coord.clone());
                if let Err(error) = &result {
                    tracing::error!(%error, "trace worker error");
                    coord.request_shutdown();
                }
                result
            }));
        }

        while !coordinator.is_shutting_down() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        wake_windows_pipe_workers(&trace_socket_path, WINDOWS_TRACE_PIPE_WORKERS);

        for worker in workers {
            let result = worker
                .join()
                .map_err(|_| GitAiError::Generic("daemon trace worker panicked".to_string()))?;
            result?;
        }

        Ok(())
    }
}

#[cfg(windows)]
fn windows_trace_pipe_worker_loop(
    trace_socket_path: PathBuf,
    mut connecting: WindowsConnectingServer,
    coordinator: Arc<ActorDaemonCoordinator>,
) -> Result<(), GitAiError> {
    loop {
        let mut server = connecting.wait().map_err(|e| {
            GitAiError::Generic(format!(
                "failed accepting trace pipe {}: {}",
                trace_socket_path.display(),
                e
            ))
        })?;

        if coordinator.is_shutting_down() {
            let _ = server.disconnect();
            break;
        }

        {
            let mut reader = BufReader::new(&mut server);
            if let Err(e) = handle_trace_connection_actor_reader(&mut reader, coordinator.clone()) {
                tracing::debug!(%e, "trace connection error");
            }
        }

        connecting = server.disconnect().map_err(|e| {
            GitAiError::Generic(format!(
                "failed recycling trace pipe {}: {}",
                trace_socket_path.display(),
                e
            ))
        })?;
    }

    Ok(())
}

#[cfg(not(windows))]
fn handle_trace_connection_actor(
    stream: LocalSocketStream,
    coordinator: Arc<ActorDaemonCoordinator>,
) -> Result<(), GitAiError> {
    let mut reader = BufReader::new(stream);
    handle_trace_connection_actor_reader(&mut reader, coordinator)
}

fn handle_trace_connection_actor_reader<R: Read>(
    reader: &mut BufReader<R>,
    coordinator: Arc<ActorDaemonCoordinator>,
) -> Result<(), GitAiError> {
    let mut observed_roots = std::collections::BTreeSet::new();
    while let Some(line) = read_json_line(reader)? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parsed: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(sid) = parsed.get("sid").and_then(Value::as_str) {
            let root_sid = trace_root_sid(sid).to_string();
            if observed_roots.insert(root_sid.clone()) {
                let _ = coordinator.trace_root_connection_opened(&root_sid);
            }
        }
        // Only enqueue payloads for mutating commands.  Read-only invocations
        // (status, diff, stash list, worktree list, …) are handled inline by
        // prepare_trace_payload_for_ingest and must not enter the serial ingest
        // queue — doing so causes the >1-minute backlog seen with IDEs that
        // issue dozens of read-only git commands per second.
        if coordinator.prepare_trace_payload_for_ingest(&mut parsed)
            && coordinator.enqueue_trace_payload(parsed).is_err()
        {
            break;
        }
    }

    if !observed_roots.is_empty() {
        let roots = observed_roots.into_iter().collect::<Vec<_>>();
        match coordinator.record_trace_connection_close(&roots) {
            Ok(stale_candidates) if !stale_candidates.is_empty() => {
                if let Err(error) =
                    coordinator.enqueue_stale_connection_close_fallbacks(&stale_candidates)
                {
                    tracing::debug!(
                        %error,
                        "trace connection close fallback error"
                    );
                }
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(
                    %error,
                    "trace connection close bookkeeping error"
                );
            }
        }
    }
    Ok(())
}

/// Git environment variables that must not leak into the daemon process.
///
/// The daemon is a long-lived, repository-agnostic process that serves requests
/// for many different repositories. Environment variables like `GIT_DIR` and
/// `GIT_WORK_TREE` pin git operations to a single repository and override the
/// `-C <path>` flag that the daemon uses to target each repository individually.
///
/// When a daemon is spawned by a git wrapper invocation (e.g. `git add`), the
/// parent process may have these variables set by git itself (hook context) or
/// by test harnesses. Clearing them at daemon startup prevents incorrect
/// repository resolution that manifests as `fatal: not a git repository: '/dev/null'`.
///
/// This list is used in two places:
/// - `spawn_daemon_run_detached` strips them from the child process via `env_remove`.
/// - `sanitize_git_env_for_daemon` clears them from the current process at daemon startup
///   as a belt-and-suspenders defence (the daemon may be launched by another mechanism).
pub const GIT_ENV_VARS_TO_SANITIZE: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_CEILING_DIRECTORIES",
    "GIT_QUARANTINE_PATH",
    "GIT_NAMESPACE",
];

fn sanitize_git_env_for_daemon() {
    for var in GIT_ENV_VARS_TO_SANITIZE {
        // SAFETY: daemon startup is single-threaded at this point -- the tokio
        // runtime is not yet running and no other threads exist.
        unsafe {
            std::env::remove_var(var);
        }
    }
}

fn disable_trace2_for_daemon_process() {
    // The daemon executes internal git commands while processing events and control requests.
    // If trace2.eventTarget points at this daemon socket globally, those internal git
    // commands can recursively feed trace2 events back into the daemon and starve progress.
    // Force-disable trace2 emission for the daemon process and all of its child git commands.
    unsafe {
        std::env::set_var("GIT_TRACE2_EVENT", "0");
    }
}

/// How often the daemon wakes up to evaluate whether an update check is due.
const DAEMON_UPDATE_CHECK_INTERVAL_SECS: u64 = 3600;

/// Maximum daemon uptime before a proactive restart (24.5 hours).
/// Deliberately offset from the 24h update-check cadence so the uptime restart
/// never races with an update-triggered shutdown.
const DAEMON_MAX_UPTIME_SECS: u64 = 24 * 3600 + 30 * 60;

/// Returns the update check interval, respecting an env var override for testing.
fn daemon_update_check_interval() -> u64 {
    std::env::var("GIT_AI_DAEMON_UPDATE_CHECK_INTERVAL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DAEMON_UPDATE_CHECK_INTERVAL_SECS)
}

/// Returns the maximum uptime in nanoseconds, respecting an env var override for testing.
fn daemon_max_uptime_ns() -> u128 {
    let secs = std::env::var("GIT_AI_DAEMON_MAX_UPTIME_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DAEMON_MAX_UPTIME_SECS);
    secs as u128 * 1_000_000_000
}

const DAEMON_SOCKET_HEALTH_CHECK_SECS: u64 = 30;

fn daemon_socket_health_check_interval() -> u64 {
    std::env::var("GIT_AI_DAEMON_SOCKET_HEALTH_CHECK_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DAEMON_SOCKET_HEALTH_CHECK_SECS)
}

/// Spawn a detached `git-ai bg restart --hard` process that will reap the
/// current (zombie) daemon and start a fresh one.  The child inherits the
/// daemon env vars (GIT_AI_DAEMON_HOME, etc.) so it targets the same
/// instance.  Returns Ok if the process was spawned; the caller should
/// still request_shutdown so the current daemon exits promptly.
fn spawn_self_restart() -> Result<(), String> {
    let exe = crate::utils::current_git_ai_exe().map_err(|e| e.to_string())?;
    tracing::info!(?exe, "spawning detached restart process");

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["bg", "restart", "--hard"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    for var in GIT_ENV_VARS_TO_SANITIZE {
        cmd.env_remove(var);
    }
    cmd.env_remove("GIT_AI");

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }

    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to spawn restart process: {}", e))
}

const DAEMON_MIN_UPTIME_FOR_SELF_RESTART_SECS: u64 = 60;

fn daemon_min_uptime_for_self_restart() -> u64 {
    std::env::var("GIT_AI_DAEMON_MIN_UPTIME_FOR_RESTART_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DAEMON_MIN_UPTIME_FOR_SELF_RESTART_SECS)
}

/// Background loop that verifies the daemon's sockets are reachable by
/// actually connecting to them.  A successful connect proves the socket file
/// exists, points to this daemon's listener, and that the listener thread is
/// alive and calling accept().  If either probe fails (deleted file, stale
/// socket, hung listener), the daemon spawns a detached restart process and
/// shuts down.
///
/// To prevent restart loops when the underlying issue is systemic (e.g.
/// filesystem permissions, broken paths), the daemon only self-restarts if
/// it has been up for at least 60 seconds.  If sockets fail before that,
/// it shuts down without restart — the next wrapper invocation will attempt
/// to start a fresh daemon.
fn daemon_socket_health_check_loop(
    coordinator: Arc<ActorDaemonCoordinator>,
    control_socket_path: PathBuf,
    trace_socket_path: PathBuf,
) {
    let started = std::time::Instant::now();
    let interval = daemon_socket_health_check_interval().max(1);
    tracing::info!(
        interval,
        control = %control_socket_path.display(),
        trace = %trace_socket_path.display(),
        "socket health check started"
    );

    loop {
        {
            let guard = coordinator
                .shutdown_condvar_mutex
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if coordinator.is_shutting_down() {
                return;
            }
            let _ = coordinator
                .shutdown_condvar
                .wait_timeout(guard, std::time::Duration::from_secs(interval));
        }

        if coordinator.is_shutting_down() {
            return;
        }

        let control_ok =
            local_socket_connects_with_timeout(&control_socket_path, DAEMON_SOCKET_PROBE_TIMEOUT);
        let trace_ok =
            local_socket_connects_with_timeout(&trace_socket_path, DAEMON_SOCKET_PROBE_TIMEOUT);

        if control_ok.is_err() || trace_ok.is_err() {
            let uptime = started.elapsed();
            let min_uptime = std::time::Duration::from_secs(daemon_min_uptime_for_self_restart());

            if uptime >= min_uptime {
                tracing::warn!(
                    control = %control_ok.err().map(|e| e.to_string()).unwrap_or_else(|| "ok".into()),
                    trace = %trace_ok.err().map(|e| e.to_string()).unwrap_or_else(|| "ok".into()),
                    "socket health check failed, spawning restart and shutting down"
                );
                if let Err(e) = spawn_self_restart() {
                    tracing::error!("failed to spawn self-restart: {}", e);
                }
            } else {
                tracing::warn!(
                    control = %control_ok.err().map(|e| e.to_string()).unwrap_or_else(|| "ok".into()),
                    trace = %trace_ok.err().map(|e| e.to_string()).unwrap_or_else(|| "ok".into()),
                    uptime_secs = uptime.as_secs(),
                    "socket health check failed within minimum uptime, shutting down without restart"
                );
            }
            coordinator.request_shutdown();
            return;
        }
    }
}

/// Background loop that periodically checks for available updates.
///
/// Sleeps in short increments so it can exit promptly when the coordinator
/// signals shutdown.  When an update is detected, it requests a graceful
/// shutdown so the daemon can self-update after draining in-flight work.
fn daemon_update_check_loop(coordinator: Arc<ActorDaemonCoordinator>, started_at_ns: u128) {
    use crate::commands::upgrade::{DaemonUpdateCheckResult, check_for_update_available};

    let interval = daemon_update_check_interval().max(1);

    loop {
        {
            let guard = coordinator
                .shutdown_condvar_mutex
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if coordinator.is_shutting_down() {
                return;
            }
            let _ = coordinator
                .shutdown_condvar
                .wait_timeout(guard, std::time::Duration::from_secs(interval));
        }

        if coordinator.is_shutting_down() {
            return;
        }

        coordinator.gc_stale_family_state();

        match check_for_update_available() {
            Ok(DaemonUpdateCheckResult::UpdateReady) => {
                tracing::info!("update check: newer version available, requesting shutdown");
                coordinator.request_restart_after_update();
                return;
            }
            Ok(DaemonUpdateCheckResult::NoUpdate) => {
                tracing::info!("update check: no update needed");
            }
            Err(err) => {
                tracing::warn!(%err, "update check failed");
            }
        }

        let uptime_ns = now_unix_nanos().saturating_sub(started_at_ns);
        if uptime_ns >= daemon_max_uptime_ns() {
            tracing::info!("uptime exceeded max, requesting restart");
            coordinator.request_restart();
            return;
        }
    }
}

/// After the daemon has fully shut down, attempt to install any pending update.
///
/// On Unix the installer atomically replaces the binary via `mv`; on Windows
/// the installer is spawned as a detached process that polls until the exe is
/// unlocked.
pub(crate) fn daemon_run_pending_self_update() -> DaemonSelfUpdateOutcome {
    use crate::commands::upgrade::{
        DaemonUpdateCheckResult, check_and_install_update_if_available,
    };

    match check_and_install_update_if_available() {
        Ok(DaemonUpdateCheckResult::UpdateReady) => {
            tracing::info!("self-update: installation completed successfully");
            DaemonSelfUpdateOutcome::Installed
        }
        Ok(DaemonUpdateCheckResult::NoUpdate) => {
            tracing::info!("self-update: no update to install");
            DaemonSelfUpdateOutcome::NoUpdate
        }
        Err(err) => {
            tracing::warn!(%err, "self-update: installation failed");
            crate::commands::upgrade::clear_cached_update_state();
            DaemonSelfUpdateOutcome::Failed
        }
    }
}

pub(crate) async fn run_daemon(config: DaemonConfig) -> Result<DaemonExitAction, GitAiError> {
    sanitize_git_env_for_daemon();
    disable_trace2_for_daemon_process();
    config.ensure_parent_dirs()?;
    remove_stale_daemon_files(&config);
    let _lock = DaemonLock::acquire(&config.lock_path)?;
    let _active_guard = DaemonProcessActiveGuard::enter();
    write_pid_metadata(&config)?;

    // Initialize tracing subscriber before log file redirect so the fmt layer
    // captures stderr (fd 2). After dup2, writes go to the daemon log file.
    {
        use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

        let env_filter = if std::env::var("GIT_AI_DEBUG").as_deref() == Ok("1") {
            EnvFilter::new("debug")
        } else {
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
        };

        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_thread_ids(false)
                    .with_ansi(false),
            )
            .with(crate::daemon::sentry_layer::SentryLayer)
            .init();
    }

    let _log_guard = maybe_setup_daemon_log_file(&config);

    tracing::info!(
        pid = std::process::id(),
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        "daemon started"
    );

    remove_socket_if_exists(&config.trace_socket_path)?;
    remove_socket_if_exists(&config.control_socket_path)?;

    let mut coordinator_inner = ActorDaemonCoordinator::new();

    // Spawn the telemetry worker inside the daemon's tokio runtime.
    let telemetry_handle = crate::daemon::telemetry_worker::spawn_telemetry_worker();
    crate::daemon::telemetry_worker::set_daemon_internal_telemetry(telemetry_handle.clone());
    coordinator_inner.telemetry_worker = Some(telemetry_handle.clone());

    // Spawn the transcript worker BEFORE wrapping coordinator in Arc
    if config::Config::get()
        .get_feature_flags()
        .transcript_streaming
    {
        let transcripts_db_path = config.internal_dir.join("transcripts-db");
        match crate::transcripts::db::TranscriptsDatabase::open(&transcripts_db_path) {
            Ok(transcripts_db) => {
                let transcripts_db = std::sync::Arc::new(transcripts_db);
                let shutdown_notify = Arc::new(tokio::sync::Notify::new());
                let transcript_handle = crate::daemon::transcript_worker::spawn_transcript_worker(
                    transcripts_db.clone(),
                    telemetry_handle.clone(),
                    shutdown_notify.clone(),
                );
                coordinator_inner.transcripts_db = Some(transcripts_db);
                coordinator_inner.transcript_worker = Some(transcript_handle);
                let _ = coordinator_inner
                    .transcript_shutdown_notify
                    .set(shutdown_notify);
                tracing::info!("transcript worker spawned");
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to open transcripts database, transcript worker not started");
            }
        }
    }

    let coordinator = Arc::new(coordinator_inner);
    coordinator.start_trace_ingest_worker()?;
    let rt_handle = tokio::runtime::Handle::current();
    let control_socket_path = config.control_socket_path.clone();
    let trace_socket_path = config.trace_socket_path.clone();

    let control_coord = coordinator.clone();
    let control_shutdown_coord = coordinator.clone();
    let control_handle = rt_handle.clone();
    let control_thread = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            control_listener_loop_actor(control_socket_path, control_coord, control_handle)
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!(%e, "control listener exited with error");
            }
            Err(_) => {
                tracing::error!("control listener panicked");
            }
        }
        // Always request shutdown so the daemon doesn't stay half-alive.
        control_shutdown_coord.request_shutdown();
    });

    let trace_coord = coordinator.clone();
    let trace_shutdown_coord = coordinator.clone();
    let trace_thread = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            trace_listener_loop_actor(trace_socket_path, trace_coord)
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!(%e, "trace listener exited with error");
            }
            Err(_) => {
                tracing::error!("trace listener panicked");
            }
        }
        // Always request shutdown so the daemon doesn't stay half-alive.
        trace_shutdown_coord.request_shutdown();
    });

    let started_at_ns = now_unix_nanos();
    let update_coord = coordinator.clone();
    let update_thread = std::thread::spawn(move || {
        daemon_update_check_loop(update_coord, started_at_ns);
    });

    let health_coord = coordinator.clone();
    let health_control = config.control_socket_path.clone();
    let health_trace = config.trace_socket_path.clone();
    let health_thread = std::thread::spawn(move || {
        daemon_socket_health_check_loop(health_coord, health_control, health_trace);
    });

    coordinator.wait_for_shutdown().await;

    // Best-effort wake listeners to allow clean process exit.
    // Connect to each socket to unblock `accept()`.  If the socket files
    // were deleted (which is exactly what the health-check detects), the
    // connection will fail — fall back to a timed join so the process still
    // exits instead of hanging forever.
    let _ = local_socket_connects_with_timeout(
        &config.control_socket_path,
        DAEMON_SOCKET_PROBE_TIMEOUT,
    );
    let _ =
        local_socket_connects_with_timeout(&config.trace_socket_path, DAEMON_SOCKET_PROBE_TIMEOUT);

    let join_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    for (name, thread) in [
        ("control", control_thread),
        ("trace", trace_thread),
        ("update", update_thread),
        ("health", health_thread),
    ] {
        let remaining = join_deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            tracing::debug!("skipping join for {} thread (deadline exceeded)", name);
            continue;
        }
        let handle = std::thread::spawn(move || {
            let _ = thread.join();
        });
        let poll_until =
            std::time::Instant::now() + remaining.min(std::time::Duration::from_millis(500));
        loop {
            if handle.is_finished() {
                let _ = handle.join();
                break;
            }
            if std::time::Instant::now() >= poll_until {
                tracing::debug!("{} thread did not join in time, proceeding", name);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    remove_socket_if_exists(&config.trace_socket_path)?;
    remove_socket_if_exists(&config.control_socket_path)?;
    remove_pid_metadata(&config)?;

    let action = coordinator.shutdown_action();
    tracing::info!(?action, "daemon shutdown complete");

    Ok(action)
}

fn checkpoint_control_timeout_uses_ci_or_test_budget() -> bool {
    std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
        || std::env::var_os("GITAI_TEST_DB_PATH").is_some()
        || std::env::var_os("CI").is_some()
}

fn checkpoint_control_response_timeout(
    request: &ControlRequest,
    use_ci_or_test_budget: bool,
) -> Duration {
    match request {
        // Queued checkpoint requests can block behind trace-ingest ordering. In
        // CI/test we allow the longer budget so replay-heavy daemon tests don't
        // tear down captured state mid-request. Product mode keeps the short
        // control timeout for fire-and-forget checkpoint requests to preserve
        // responsiveness.
        ControlRequest::CheckpointRun { .. } if use_ci_or_test_budget => {
            DAEMON_CHECKPOINT_RESPONSE_TIMEOUT
        }
        ControlRequest::CheckpointRun { .. } => DAEMON_CONTROL_RESPONSE_TIMEOUT,
        ControlRequest::SnapshotWatermarks { .. } => Duration::from_millis(500),
        _ => DAEMON_CONTROL_RESPONSE_TIMEOUT,
    }
}

fn control_request_response_timeout(request: &ControlRequest) -> Duration {
    checkpoint_control_response_timeout(
        request,
        checkpoint_control_timeout_uses_ci_or_test_budget(),
    )
}

#[cfg(not(windows))]
fn local_socket_name<'a>(socket_path: &'a Path) -> Result<Name<'a>, GitAiError> {
    socket_path
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| GitAiError::Generic(format!("invalid daemon socket path: {}", e)))
}

pub fn open_local_socket_stream_with_timeout(
    socket_path: &Path,
    timeout: Duration,
) -> Result<DaemonClientStream, GitAiError> {
    #[cfg(windows)]
    {
        let stream = open_windows_named_pipe_client_with_timeout(socket_path, timeout)?;
        Ok(DaemonClientStream::WindowsPipe(stream))
    }

    #[cfg(not(windows))]
    {
        ConnectOptions::new()
            .name(local_socket_name(socket_path)?)
            .wait_mode(ConnectWaitMode::Timeout(timeout))
            .connect_sync()
            .map_err(|e| {
                GitAiError::Generic(format!(
                    "timed out after {:?} connecting daemon socket {}: {}",
                    timeout,
                    socket_path.display(),
                    e
                ))
            })
    }
}

#[cfg(windows)]
fn open_windows_named_pipe_client_with_timeout(
    socket_path: &Path,
    timeout: Duration,
) -> Result<WindowsPipeClient, GitAiError> {
    let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
    WindowsPipeClient::connect_ms(socket_path.as_os_str(), timeout_ms).map_err(|e| {
        GitAiError::Generic(format!(
            "timed out after {:?} connecting daemon socket {}: {}",
            timeout,
            socket_path.display(),
            e
        ))
    })
}

fn set_daemon_client_stream_timeouts(
    stream: &mut DaemonClientStream,
    socket_path: &Path,
    timeout: Duration,
) -> Result<(), GitAiError> {
    #[cfg(windows)]
    {
        let _ = socket_path;
        match stream {
            DaemonClientStream::WindowsPipe(pipe) => {
                pipe.set_read_timeout(Some(timeout));
                pipe.set_write_timeout(Some(timeout));
                Ok(())
            }
        }
    }

    #[cfg(not(windows))]
    {
        stream.set_recv_timeout(Some(timeout)).map_err(|e| {
            GitAiError::Generic(format!(
                "failed to set daemon socket {} recv timeout: {}",
                socket_path.display(),
                e
            ))
        })?;
        stream.set_send_timeout(Some(timeout)).map_err(|e| {
            GitAiError::Generic(format!(
                "failed to set daemon socket {} send timeout: {}",
                socket_path.display(),
                e
            ))
        })
    }
}

fn write_all_daemon_client_stream(
    stream: &mut DaemonClientStream,
    socket_path: &Path,
    payload: &[u8],
) -> Result<(), GitAiError> {
    stream.write_all(payload).map_err(|e| {
        GitAiError::Generic(format!(
            "failed writing daemon request to {}: {}",
            socket_path.display(),
            e
        ))
    })?;
    stream.flush().map_err(|e| {
        GitAiError::Generic(format!(
            "failed flushing daemon request to {}: {}",
            socket_path.display(),
            e
        ))
    })?;
    Ok(())
}

fn read_daemon_client_line(
    reader: &mut BufReader<DaemonClientStream>,
    socket_path: &Path,
) -> Result<String, GitAiError> {
    let mut line = String::new();
    let read = reader.read_line(&mut line).map_err(|e| {
        GitAiError::Generic(format!(
            "failed reading daemon response from {}: {}",
            socket_path.display(),
            e
        ))
    })?;
    if read == 0 {
        return Err(GitAiError::Generic(format!(
            "daemon socket {} closed without a response",
            socket_path.display()
        )));
    }
    Ok(line)
}

#[cfg(windows)]
fn send_control_request_with_timeouts_windows(
    socket_path: &Path,
    request: &ControlRequest,
    connect_timeout: Duration,
    response_timeout: Duration,
) -> Result<ControlResponse, GitAiError> {
    let mut stream = open_local_socket_stream_with_timeout(socket_path, connect_timeout)?;
    set_daemon_client_stream_timeouts(&mut stream, socket_path, response_timeout)?;
    let mut body = serde_json::to_vec(request)?;
    body.push(b'\n');
    write_all_daemon_client_stream(&mut stream, socket_path, &body)?;

    let mut response_reader = BufReader::new(stream);
    let line = read_daemon_client_line(&mut response_reader, socket_path)?;
    if line.trim().is_empty() {
        return Err(GitAiError::Generic(
            "empty daemon control response".to_string(),
        ));
    }
    serde_json::from_str(line.trim()).map_err(GitAiError::from)
}

#[cfg(not(windows))]
fn send_control_request_with_timeouts_unix(
    socket_path: &Path,
    request: &ControlRequest,
    connect_timeout: Duration,
    response_timeout: Duration,
) -> Result<ControlResponse, GitAiError> {
    let mut stream = open_local_socket_stream_with_timeout(socket_path, connect_timeout)?;
    set_daemon_client_stream_timeouts(&mut stream, socket_path, response_timeout)?;
    let mut body = serde_json::to_vec(request)?;
    body.push(b'\n');
    write_all_daemon_client_stream(&mut stream, socket_path, &body)?;

    let mut response_reader = BufReader::new(stream);
    let line = read_daemon_client_line(&mut response_reader, socket_path)?;
    if line.trim().is_empty() {
        return Err(GitAiError::Generic(
            "empty daemon control response".to_string(),
        ));
    }
    serde_json::from_str(line.trim()).map_err(GitAiError::from)
}

pub fn local_socket_connects_with_timeout(
    socket_path: &Path,
    timeout: Duration,
) -> Result<(), GitAiError> {
    let _stream = open_local_socket_stream_with_timeout(socket_path, timeout)?;
    Ok(())
}

pub fn send_control_request_with_timeout(
    socket_path: &Path,
    request: &ControlRequest,
    timeout: Duration,
) -> Result<ControlResponse, GitAiError> {
    send_control_request_with_timeouts(socket_path, request, timeout, timeout)
}

fn send_control_request_with_timeouts(
    socket_path: &Path,
    request: &ControlRequest,
    connect_timeout: Duration,
    response_timeout: Duration,
) -> Result<ControlResponse, GitAiError> {
    #[cfg(windows)]
    {
        send_control_request_with_timeouts_windows(
            socket_path,
            request,
            connect_timeout,
            response_timeout,
        )
    }

    #[cfg(not(windows))]
    {
        send_control_request_with_timeouts_unix(
            socket_path,
            request,
            connect_timeout,
            response_timeout,
        )
    }
}

pub fn send_control_request(
    socket_path: &Path,
    request: &ControlRequest,
) -> Result<ControlResponse, GitAiError> {
    send_control_request_with_timeouts(
        socket_path,
        request,
        DAEMON_CONTROL_CONNECT_TIMEOUT,
        control_request_response_timeout(request),
    )
}

pub fn send_control_request_fire_and_forget(
    socket_path: &Path,
    request: &ControlRequest,
) -> Result<(), GitAiError> {
    let mut stream =
        open_local_socket_stream_with_timeout(socket_path, DAEMON_CONTROL_CONNECT_TIMEOUT)?;
    let write_timeout = Duration::from_millis(500);
    set_daemon_client_stream_timeouts(&mut stream, socket_path, write_timeout)?;
    let mut body = serde_json::to_vec(request)?;
    body.push(b'\n');
    write_all_daemon_client_stream(&mut stream, socket_path, &body)?;
    Ok(())
}

#[cfg(test)]
mod transcript_worker_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::OsString;

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: these tests are serialized via #[serial], so mutating the
            // process environment is isolated for the duration of each test.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }

        fn unset(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: these tests are serialized via #[serial], so mutating the
            // process environment is isolated for the duration of each test.
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => {
                    // SAFETY: these tests are serialized via #[serial], so restoring
                    // process environment state is isolated for the duration of each test.
                    unsafe {
                        std::env::set_var(self.key, value);
                    }
                }
                None => {
                    // SAFETY: these tests are serialized via #[serial], so restoring
                    // process environment state is isolated for the duration of each test.
                    unsafe {
                        std::env::remove_var(self.key);
                    }
                }
            }
        }
    }

    fn sample_checkpoint_request() -> ControlRequest {
        use crate::commands::checkpoint_agent::orchestrator::{BaseCommit, CheckpointFile};
        ControlRequest::CheckpointRun {
            request: Box::new(CheckpointRequest {
                trace_id: "test-trace".to_string(),
                checkpoint_kind: CheckpointKind::Human,
                agent_id: None,
                files: vec![CheckpointFile {
                    path: std::path::PathBuf::from("test.txt"),
                    content: None,
                    repo_work_dir: std::path::PathBuf::from("/tmp/repo"),
                    base_commit: BaseCommit::Initial,
                }],
                path_role: PreparedPathRole::WillEdit,
                transcript_source: None,
                metadata: std::collections::HashMap::new(),
            }),
        }
    }

    #[test]
    fn checkpoint_requests_use_long_timeout_in_ci_or_test_env() {
        assert_eq!(
            checkpoint_control_response_timeout(&sample_checkpoint_request(), true),
            DAEMON_CHECKPOINT_RESPONSE_TIMEOUT
        );
    }

    #[test]
    fn checkpoint_requests_use_short_timeout_in_product_env() {
        assert_eq!(
            checkpoint_control_response_timeout(&sample_checkpoint_request(), false),
            DAEMON_CONTROL_RESPONSE_TIMEOUT
        );
    }

    #[test]
    #[serial]
    fn checkpoint_control_timeout_uses_ci_env_var() {
        let _unset_test = EnvVarGuard::unset("GIT_AI_TEST_DB_PATH");
        let _unset_legacy_test = EnvVarGuard::unset("GITAI_TEST_DB_PATH");
        let _set_ci = EnvVarGuard::set("CI", "true");

        assert!(checkpoint_control_timeout_uses_ci_or_test_budget());
    }

    #[test]
    #[serial]
    fn checkpoint_control_timeout_uses_test_db_env_var() {
        let _unset_ci = EnvVarGuard::unset("CI");
        let _unset_legacy_test = EnvVarGuard::unset("GITAI_TEST_DB_PATH");
        let _set_test = EnvVarGuard::set("GIT_AI_TEST_DB_PATH", "/tmp/git-ai-test.db");

        assert!(checkpoint_control_timeout_uses_ci_or_test_budget());
    }

    #[test]
    #[serial]
    fn checkpoint_control_timeout_false_when_no_ci_or_test_vars() {
        let _unset_ci = EnvVarGuard::unset("CI");
        let _unset_test = EnvVarGuard::unset("GIT_AI_TEST_DB_PATH");
        let _unset_legacy_test = EnvVarGuard::unset("GITAI_TEST_DB_PATH");

        assert!(!checkpoint_control_timeout_uses_ci_or_test_budget());
    }

    #[test]
    fn compute_watermarks_uses_symlink_metadata_not_target_mtime() {
        // Verify that compute_watermarks_from_stat uses lstat (symlink's own mtime)
        // not stat (target file's mtime), consistent with snapshot's symlink_metadata.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Create a target file
        let target = dir.join("target.txt");
        std::fs::write(&target, b"hello").unwrap();

        // Create a symlink pointing to the target
        let link = dir.join("link.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &link).unwrap();

        // Watermark the symlink
        let wm = compute_watermarks_from_stat(dir.to_str().unwrap(), &["link.txt".to_string()]);

        // The watermark should match symlink_metadata mtime, not target metadata mtime.
        let symlink_meta = std::fs::symlink_metadata(&link).unwrap();
        let target_meta = std::fs::metadata(&link).unwrap(); // follows symlink

        let symlink_mtime = symlink_meta
            .modified()
            .unwrap()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let target_mtime = target_meta
            .modified()
            .unwrap()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let recorded = *wm.get("link.txt").unwrap();

        assert_eq!(
            recorded, symlink_mtime,
            "watermark should match lstat mtime of the symlink itself"
        );
        // This assertion documents the intent: if symlink and target mtimes differ,
        // the watermark must track the symlink, not the target.
        let _ = target_mtime; // used only as documentation; may equal symlink_mtime on some FS
    }

    #[test]
    fn explicit_stop_overrides_prior_restart_intent() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let coordinator = ActorDaemonCoordinator::new();

            coordinator.request_restart_after_update();
            assert_eq!(
                coordinator.shutdown_action(),
                DaemonExitAction::RestartAfterUpdate
            );

            coordinator.request_stop();

            assert!(coordinator.is_shutting_down());
            assert_eq!(coordinator.shutdown_action(), DaemonExitAction::Stop);
        });
    }

    // -----------------------------------------------------------------------
    // Readonly command ingress fast-path tests
    //
    // These tests verify that prepare_trace_payload_for_ingest returns false
    // (do-not-enqueue) for read-only commands and true for mutating ones, and
    // that the queued_trace_payloads counter is not incremented for read-only
    // events.
    //
    // ActorDaemonCoordinator::new() spawns Tokio tasks internally, so all
    // tests that construct one must run inside a Tokio runtime.
    // -----------------------------------------------------------------------

    fn make_start_payload(argv: &[&str]) -> Value {
        serde_json::json!({
            "event": "start",
            "sid": "20260411T120000.000000-Psid1",
            "argv": argv,
        })
    }

    fn make_atexit_payload(sid: &str) -> Value {
        serde_json::json!({
            "event": "atexit",
            "sid": sid,
            "code": 0,
        })
    }

    #[tokio::test]
    async fn readonly_start_event_is_not_enqueued() {
        let coord = ActorDaemonCoordinator::new();
        let mut payload = make_start_payload(&["git", "status", "--short"]);
        let should_enqueue = coord.prepare_trace_payload_for_ingest(&mut payload);
        assert!(
            !should_enqueue,
            "status start event should not be enqueued (readonly)"
        );
        assert_eq!(
            coord.queued_trace_payloads.load(Ordering::Relaxed),
            0,
            "queued_trace_payloads should stay 0 for readonly start event"
        );
        // Readonly events must NOT receive an ingest sequence number
        assert!(
            payload.get(TRACE_INGEST_SEQ_FIELD).is_none(),
            "readonly start event must not receive an ingest sequence number"
        );
    }

    #[tokio::test]
    async fn stash_list_start_event_is_not_enqueued() {
        let coord = ActorDaemonCoordinator::new();
        let mut payload = make_start_payload(&[
            "git",
            "-c",
            "core.fsmonitor=false",
            "--no-pager",
            "stash",
            "list",
            "--pretty=format:%gd%x00%H%x00%ct%x00%s",
        ]);
        let should_enqueue = coord.prepare_trace_payload_for_ingest(&mut payload);
        assert!(
            !should_enqueue,
            "stash list start event should not be enqueued (readonly invocation)"
        );
        assert!(
            payload.get(TRACE_INGEST_SEQ_FIELD).is_none(),
            "stash list start event must not receive an ingest sequence number"
        );
    }

    #[tokio::test]
    async fn worktree_list_start_event_is_not_enqueued() {
        let coord = ActorDaemonCoordinator::new();
        let mut payload = make_start_payload(&[
            "git",
            "--no-pager",
            "--no-optional-locks",
            "worktree",
            "list",
            "--porcelain",
        ]);
        let should_enqueue = coord.prepare_trace_payload_for_ingest(&mut payload);
        assert!(
            !should_enqueue,
            "worktree list start event should not be enqueued (readonly invocation)"
        );
        assert!(
            payload.get(TRACE_INGEST_SEQ_FIELD).is_none(),
            "worktree list start event must not receive an ingest sequence number"
        );
    }

    #[tokio::test]
    async fn diff_numstat_start_event_is_not_enqueued() {
        let coord = ActorDaemonCoordinator::new();
        let mut payload = make_start_payload(&[
            "git",
            "-c",
            "core.fsmonitor=false",
            "--no-pager",
            "diff",
            "--numstat",
            "--no-renames",
            "HEAD",
        ]);
        let should_enqueue = coord.prepare_trace_payload_for_ingest(&mut payload);
        assert!(
            !should_enqueue,
            "diff --numstat start event should not be enqueued"
        );
    }

    #[tokio::test]
    async fn for_each_ref_start_event_is_not_enqueued() {
        let coord = ActorDaemonCoordinator::new();
        let mut payload = make_start_payload(&[
            "git",
            "--no-pager",
            "for-each-ref",
            "refs/heads/**/*",
            "refs/remotes/**/*",
            "--format",
            "%(HEAD)%00%(objectname)",
        ]);
        let should_enqueue = coord.prepare_trace_payload_for_ingest(&mut payload);
        assert!(
            !should_enqueue,
            "for-each-ref start event should not be enqueued"
        );
    }

    #[tokio::test]
    async fn cat_file_start_event_is_not_enqueued() {
        let coord = ActorDaemonCoordinator::new();
        let mut payload = make_start_payload(&[
            "git",
            "--no-optional-locks",
            "cat-file",
            "--batch-check=%(objectname)",
        ]);
        let should_enqueue = coord.prepare_trace_payload_for_ingest(&mut payload);
        assert!(
            !should_enqueue,
            "cat-file start event should not be enqueued"
        );
    }

    #[tokio::test]
    async fn show_commit_start_event_is_not_enqueued() {
        let coord = ActorDaemonCoordinator::new();
        let mut payload = make_start_payload(&[
            "git",
            "--no-optional-locks",
            "show",
            "--no-patch",
            "--format=%H%x00%B%x00%at",
            "07270e1489439d6b36fcb2a4198d2fb68e37727c",
        ]);
        let should_enqueue = coord.prepare_trace_payload_for_ingest(&mut payload);
        assert!(!should_enqueue, "show start event should not be enqueued");
    }

    #[tokio::test]
    async fn mutating_commit_start_event_is_enqueued() {
        let coord = ActorDaemonCoordinator::new();
        let mut payload = make_start_payload(&["git", "commit", "-m", "test commit"]);
        let should_enqueue = coord.prepare_trace_payload_for_ingest(&mut payload);
        assert!(
            should_enqueue,
            "commit start event should be enqueued (mutating)"
        );
        assert!(
            payload.get(TRACE_INGEST_SEQ_FIELD).is_some(),
            "mutating event must receive an ingest sequence number"
        );
    }

    #[tokio::test]
    async fn mutating_stash_pop_start_event_is_enqueued() {
        let coord = ActorDaemonCoordinator::new();
        let mut payload = make_start_payload(&["git", "stash", "pop"]);
        let should_enqueue = coord.prepare_trace_payload_for_ingest(&mut payload);
        assert!(
            should_enqueue,
            "stash pop start event should be enqueued (mutating)"
        );
    }

    #[tokio::test]
    async fn mutating_worktree_add_start_event_is_enqueued() {
        let coord = ActorDaemonCoordinator::new();
        let mut payload = make_start_payload(&["git", "worktree", "add", "/tmp/branch", "branch"]);
        let should_enqueue = coord.prepare_trace_payload_for_ingest(&mut payload);
        assert!(
            should_enqueue,
            "worktree add start event should be enqueued (mutating)"
        );
    }

    #[tokio::test]
    async fn readonly_atexit_event_is_not_enqueued_after_readonly_start() {
        let coord = ActorDaemonCoordinator::new();
        let sid = "20260411T120000.000000-Psid1";

        // Process start event first — marks root as read-only
        let mut start = make_start_payload(&["git", "status"]);
        // Override sid to match
        start["sid"] = serde_json::json!(sid);
        coord.prepare_trace_payload_for_ingest(&mut start);

        // atexit for same root should also be skipped
        let mut atexit = make_atexit_payload(sid);
        let should_enqueue = coord.prepare_trace_payload_for_ingest(&mut atexit);
        assert!(
            !should_enqueue,
            "atexit for readonly root should not be enqueued"
        );
    }

    /// Performance invariant: 10,000 readonly start events must be processed
    /// (and discarded) in under 200ms.  This guards against regressions that
    /// re-introduce the >1-minute backlog seen with Zed's ~40 invocations/sec.
    #[tokio::test]
    async fn readonly_flood_1000_events_processed_in_under_200ms() {
        let coord = ActorDaemonCoordinator::new();
        let start = std::time::Instant::now();
        for i in 0..1000u64 {
            let sid = format!("20260411T120000.000000-P{:016x}", i);
            let mut payload = serde_json::json!({
                "event": "start",
                "sid": sid,
                "argv": ["git", "-c", "core.fsmonitor=false", "--no-pager",
                         "--no-optional-locks", "status", "--porcelain=v1",
                         "--untracked-files=all", "--no-renames", "-z", "."],
            });
            let enqueue = coord.prepare_trace_payload_for_ingest(&mut payload);
            assert!(!enqueue, "status must never be enqueued");
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 200,
            "processing 1000 readonly events took {}ms (> 200ms budget)",
            elapsed.as_millis()
        );
        assert_eq!(
            coord.queued_trace_payloads.load(Ordering::Relaxed),
            0,
            "no readonly events should reach the ingest queue"
        );
    }

    /// Ensure a stash-list flood (3208 real-world invocations from Zed)
    /// leaves the ingest queue empty.
    #[tokio::test]
    async fn stash_list_flood_leaves_queue_empty() {
        let coord = ActorDaemonCoordinator::new();
        for i in 0..1000u64 {
            let sid = format!("20260411T120000.000000-P{:016x}", i);
            let mut payload = serde_json::json!({
                "event": "start",
                "sid": sid,
                "argv": ["git", "-c", "core.fsmonitor=false", "--no-pager",
                         "stash", "list", "--pretty=format:%gd%x00%H%x00%ct%x00%s"],
            });
            let _ = coord.prepare_trace_payload_for_ingest(&mut payload);
        }
        assert_eq!(
            coord.queued_trace_payloads.load(Ordering::Relaxed),
            0,
            "stash list flood must not fill the ingest queue"
        );
    }

    /// Ensure a worktree-list flood leaves the ingest queue empty.
    #[tokio::test]
    async fn worktree_list_flood_leaves_queue_empty() {
        let coord = ActorDaemonCoordinator::new();
        for i in 0..1000u64 {
            let sid = format!("20260411T120000.000000-P{:016x}", i);
            let mut payload = serde_json::json!({
                "event": "start",
                "sid": sid,
                "argv": ["git", "--no-pager", "--no-optional-locks",
                         "worktree", "list", "--porcelain"],
            });
            let _ = coord.prepare_trace_payload_for_ingest(&mut payload);
        }
        assert_eq!(
            coord.queued_trace_payloads.load(Ordering::Relaxed),
            0,
            "worktree list flood must not fill the ingest queue"
        );
    }

    // -----------------------------------------------------------------------
    // OnceLock / shutdown / atomic-ordering tests
    // -----------------------------------------------------------------------

    /// `enqueue_trace_payload` must return an error when the ingest worker has
    /// not been started yet.  This is the "no-sender" fast-fail path and is
    /// unchanged by the OnceLock refactor.
    #[tokio::test]
    async fn enqueue_before_worker_start_returns_error() {
        let coord = ActorDaemonCoordinator::new();
        // Worker never started → OnceLock is empty → enqueue must fail
        let payload = serde_json::json!({
            "event": "start",
            "sid": "20260411T120000.000000-Ptest0001",
            "__git_ai_ingest_seq": 1_u64,
            "argv": ["git", "commit", "-m", "test"],
        });
        assert!(
            coord.enqueue_trace_payload(payload).is_err(),
            "enqueue before worker start must return an error"
        );
    }

    /// After `request_shutdown()`, `is_shutting_down()` returns true and the
    /// coordinator stays in a consistent state.  The ingest worker (started
    /// via `start_trace_ingest_worker`) must exit cleanly even when the sender
    /// is no longer dropped by `request_shutdown` (OnceLock never drops it).
    #[tokio::test]
    async fn request_shutdown_is_idempotent_and_consistent() {
        let coord = Arc::new(ActorDaemonCoordinator::new());
        coord.start_trace_ingest_worker().unwrap();
        assert!(!coord.is_shutting_down());
        coord.request_shutdown();
        assert!(coord.is_shutting_down());
        // Second call must not panic.
        coord.request_shutdown();
        assert!(coord.is_shutting_down());
        // Allow tokio to run the ingest worker's shutdown select arm.
        tokio::task::yield_now().await;
    }

    /// Concurrent enqueues from multiple threads must never deadlock or
    /// corrupt the accounting counter.
    #[tokio::test]
    async fn concurrent_mutating_enqueues_do_not_deadlock() {
        use std::sync::Arc;
        let coord = Arc::new(ActorDaemonCoordinator::new());
        coord.start_trace_ingest_worker().unwrap();

        const TASKS: usize = 8;
        const PER_TASK: usize = 20;

        // Use prepare_trace_payload_for_ingest (which allocates seq numbers
        // and enqueues) from multiple tasks concurrently.
        let mut handles = Vec::with_capacity(TASKS);
        for task_id in 0..TASKS {
            let c = coord.clone();
            handles.push(tokio::spawn(async move {
                for i in 0..PER_TASK {
                    let sid = format!("20260411T120000.000000-P{:08x}", task_id * 1000 + i);
                    let mut payload = serde_json::json!({
                        "event": "start",
                        "sid": sid,
                        "argv": ["git", "commit", "-m", "msg"],
                    });
                    // This calls enqueue_trace_payload internally for mutating cmds.
                    let _ = c.prepare_trace_payload_for_ingest(&mut payload);
                }
            }));
        }
        for h in handles {
            h.await.expect("task must not panic");
        }
        // Give the ingest worker time to drain the queue.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while coord.queued_trace_payloads.load(Ordering::Acquire) > 0 {
            if tokio::time::Instant::now() >= deadline {
                break; // don't fail the test on CI slowness; just stop waiting
            }
            tokio::task::yield_now().await;
        }
        coord.request_shutdown();
    }
}
