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
    read_head_state_for_worktree, read_ref_oid_for_worktree,
    resolve_linear_head_commit_chain_for_worktree, resolve_rebase_segment_for_worktree,
    resolve_reflog_old_oid_for_ref_new_oid_in_worktree, resolve_squash_source_head_for_worktree,
    resolve_stash_target_oid_for_worktree, resolve_worktree_head_reflog_old_oid_for_new_head,
    worktree_root_for_path,
};
use crate::git::repository::{Repository, discover_repository_in_path_no_git_exec, exec_git};
use crate::git::rewrite_log::{
    CherryPickAbortEvent, CherryPickCompleteEvent, MergeSquashEvent, RebaseAbortEvent,
    RebaseCompleteEvent, ResetEvent, ResetKind, RewriteLogEvent, StashEvent, StashOperation,
};
use crate::git::sync_authorship::{fetch_authorship_notes, fetch_remote_from_args};
use crate::observability;
use crate::utils::{LockFile, debug_log};
use crate::{
    authorship::post_commit::post_commit_with_final_state,
    authorship::rebase_authorship::{
        committed_file_snapshot_between_commits, prepare_working_log_after_squash_from_final_state,
        reconstruct_working_log_after_reset, restore_virtual_attribution_carryover,
        restore_working_log_carryover, rewrite_authorship_after_commit_amend_with_snapshot,
        rewrite_authorship_if_needed,
    },
    authorship::working_log::CheckpointKind,
    commands::checkpoint_agent::agent_presets::AgentRunResult,
    commands::hooks::{push_hooks, stash_hooks},
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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc, oneshot};
use tokio::time::Duration;

pub mod analyzers;
pub mod control_api;
pub mod coordinator;
pub mod domain;
pub mod family_actor;
pub mod git_backend;
pub mod global_actor;
pub mod reducer;
pub mod telemetry_handle;
pub mod telemetry_worker;
pub mod test_sync;
pub mod trace_normalizer;

pub use control_api::{
    CapturedCheckpointRunRequest, CheckpointRunRequest, ControlRequest, ControlResponse,
    FamilyStatus, LiveCheckpointRunRequest, TelemetryEnvelope,
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
static DAEMON_PROCESS_ACTIVE: AtomicBool = AtomicBool::new(false);

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

fn trace_command_is_definitely_read_only(primary_command: Option<&str>) -> bool {
    use crate::git::command_classification::is_definitely_read_only_command;
    match primary_command {
        Some(cmd) => is_definitely_read_only_command(cmd),
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

fn commit_replay_files_from_snapshot(snapshot: &HashMap<String, String>) -> Vec<String> {
    let mut files = snapshot.keys().cloned().collect::<Vec<_>>();
    files.sort();
    files
}

fn stable_final_state_for_commit_rewrite(
    repo: &Repository,
    rewrite_event: &RewriteLogEvent,
) -> Result<Option<HashMap<String, String>>, GitAiError> {
    let Some((base_commit, target_commit)) =
        commit_replay_context_from_rewrite_event(rewrite_event)
    else {
        return Ok(None);
    };
    if base_commit.trim().is_empty() || target_commit.trim().is_empty() {
        return Ok(None);
    }

    committed_file_snapshot_between_commits(
        repo,
        if base_commit == "initial" {
            None
        } else {
            Some(base_commit.as_str())
        },
        &target_commit,
    )
    .map(Some)
}

fn exact_final_state_for_commit_replay(
    repo: &Repository,
    rewrite_event: &RewriteLogEvent,
    carryover_snapshot: Option<&HashMap<String, String>>,
) -> Result<Option<HashMap<String, String>>, GitAiError> {
    let mut final_state =
        stable_final_state_for_commit_rewrite(repo, rewrite_event)?.unwrap_or_default();
    if let Some(snapshot) = carryover_snapshot {
        final_state.extend(snapshot.clone());
    }
    if final_state.is_empty() {
        return Ok(None);
    }
    Ok(Some(final_state))
}

fn normalize_line_endings_for_snapshot_compare(content: &str) -> std::borrow::Cow<'_, str> {
    if !content.contains('\r') {
        return std::borrow::Cow::Borrowed(content);
    }
    std::borrow::Cow::Owned(content.replace("\r\n", "\n").replace('\r', "\n"))
}

fn normalize_commit_carryover_snapshot(
    carryover_snapshot: Option<&HashMap<String, String>>,
    committed_final_state: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    let carryover_snapshot = carryover_snapshot?;

    let mut normalized = carryover_snapshot.clone();
    if let Some(committed_final_state) = committed_final_state {
        for (file_path, committed_content) in committed_final_state {
            if let Some(snapshot_content) = normalized.get_mut(file_path)
                && normalize_line_endings_for_snapshot_compare(snapshot_content)
                    == normalize_line_endings_for_snapshot_compare(committed_content)
            {
                *snapshot_content = committed_content.clone();
            }
        }
    }

    Some(normalized)
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

fn inferred_top_stash_sha_from_rewrite_history(
    worktree: &Path,
) -> Result<Option<String>, GitAiError> {
    let repo = discover_repository_in_path_no_git_exec(worktree)?;
    let events = repo.storage.read_rewrite_events()?;
    let mut stack: Vec<String> = Vec::new();
    for event in events {
        let RewriteLogEvent::Stash { stash } = event else {
            continue;
        };
        if !stash.success {
            continue;
        }
        match stash.operation {
            StashOperation::Create => {
                if let Some(stash_sha) = stash
                    .stash_sha
                    .filter(|stash_sha| !stash_sha.is_empty() && !is_zero_oid(stash_sha))
                {
                    stack.push(stash_sha);
                }
            }
            StashOperation::Pop | StashOperation::Drop => {
                if let Some(stash_sha) = stash.stash_sha
                    && let Some(position) =
                        stack.iter().rposition(|existing| existing == &stash_sha)
                {
                    stack.remove(position);
                    continue;
                }
                if stash_target_spec_is_top_of_stack(stash.stash_ref.as_deref()) {
                    let _ = stack.pop();
                }
            }
            StashOperation::Apply | StashOperation::List => {}
        }
    }
    Ok(stack.last().cloned())
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
    if !stash_requires_target_resolution(&parsed.command_args) {
        return Ok(None);
    }

    let target_spec = stash_target_spec(&parsed.command_args);
    match parsed.command_args.first().map(String::as_str).unwrap_or("push") {
        "apply" => resolve_stash_target_oid_for_worktree(worktree, target_spec)
            .ok_or_else(|| {
                GitAiError::Generic(format!(
                    "failed to resolve stash apply target oid from terminal repo state (spec={:?}, worktree={})",
                    target_spec,
                    worktree.display()
                ))
            })
            .map(Some),
        "pop" | "drop" => {
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

type MergeSquashSnapshot = String;
type DeferredCommitCarryover = (
    String,
    crate::authorship::virtual_attribution::VirtualAttributions,
    HashMap<String, String>,
);

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

fn apply_checkpoint_side_effect(request: CheckpointRunRequest) -> Result<(), GitAiError> {
    match request {
        CheckpointRunRequest::Live(request) => {
            let repo = find_repository_in_path(&request.repo_working_dir)?;
            crate::commands::git_hook_handlers::ensure_repo_level_hooks_for_checkpoint(&repo);

            let kind = request
                .kind
                .as_deref()
                .and_then(parse_checkpoint_kind)
                .or_else(|| request.agent_run_result.as_ref().map(|r| r.checkpoint_kind))
                .unwrap_or(CheckpointKind::Human);
            let author = request
                .author
                .unwrap_or_else(|| repo.git_author_identity().name_or_unknown());

            let _ = crate::commands::checkpoint::run(
                &repo,
                &author,
                kind,
                request.reset.unwrap_or(false),
                request.quiet.unwrap_or(true),
                request.agent_run_result,
                request.is_pre_commit.unwrap_or(false),
            )?;
            Ok(())
        }
        CheckpointRunRequest::Captured(request) => {
            let repo = find_repository_in_path(&request.repo_working_dir)?;
            crate::commands::git_hook_handlers::ensure_repo_level_hooks_for_checkpoint(&repo);
            let _ = crate::commands::checkpoint::execute_captured_checkpoint(
                &repo,
                &request.capture_id,
            )?;
            Ok(())
        }
    }
}

fn parse_checkpoint_kind(value: &str) -> Option<CheckpointKind> {
    match value {
        "human" => Some(CheckpointKind::Human),
        "ai_agent" => Some(CheckpointKind::AiAgent),
        "ai_tab" => Some(CheckpointKind::AiTab),
        _ => None,
    }
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
    let repo = find_repository_in_path(worktree)?;
    let parsed = parsed_invocation_for_side_effect(command, args);
    push_hooks::run_pre_push_hook_managed(&parsed, &repo);
    Ok(())
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
            debug_log(&format!(
                "daemon notes sync: failed to determine remote for {}: {}",
                parsed.command.as_deref().unwrap_or("pull"),
                error
            ));
            return Ok(());
        }
    };
    if let Err(error) = fetch_authorship_notes(&repo, &remote) {
        debug_log(&format!(
            "daemon notes sync: failed to fetch authorship notes from {}: {}",
            remote, error
        ));
    }
    Ok(())
}

fn apply_clone_notes_sync_side_effect(worktree: &str) -> Result<(), GitAiError> {
    let repo = find_repository_in_path(worktree)?;
    if let Err(error) = fetch_authorship_notes(&repo, "origin") {
        debug_log(&format!(
            "daemon notes sync: failed to fetch clone authorship notes from origin: {}",
            error
        ));
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
        .unwrap_or_default()
        .to_string();

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
            return Err(GitAiError::Generic(format!(
                "{} --merge missing captured carryover snapshot",
                cmd.primary_command.as_deref().unwrap_or("checkout")
            )));
        }
        if let Some(snapshot) = carryover_snapshot {
            restore_working_log_carryover(
                &repo,
                &old_head,
                &new_head,
                snapshot.clone(),
                Some(repo.git_author_identity().name_or_unknown()),
            )?;
        }
        repo.storage.delete_working_log_for_base_commit(&old_head)?;
        return Ok(());
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
            (!snapshot.is_empty()).then(|| RecentReplayPrerequisite::CheckoutSwitchMerge {
                target_head: new_head,
                old_head,
                final_state: snapshot.clone(),
            })
        });
    }

    Some(RecentReplayPrerequisite::CheckoutSwitchRename {
        target_head: new_head,
        old_head,
    })
}

fn commit_replay_context_from_rewrite_event(
    rewrite_event: &RewriteLogEvent,
) -> Option<(String, String)> {
    match rewrite_event {
        RewriteLogEvent::Commit { commit } => {
            let base_commit = commit
                .base_commit
                .as_deref()
                .filter(|sha| {
                    let trimmed = sha.trim();
                    !trimmed.is_empty() && !is_zero_oid(trimmed)
                })
                .unwrap_or("initial")
                .to_string();
            Some((base_commit, commit.commit_sha.clone()))
        }
        RewriteLogEvent::CommitAmend { commit_amend } => Some((
            commit_amend.original_commit.clone(),
            commit_amend.amended_commit_sha.clone(),
        )),
        _ => None,
    }
}

fn filter_commit_replay_files(
    working_log: &crate::git::repo_storage::PersistedWorkingLog,
    files: Vec<String>,
    dirty_files: HashMap<String, String>,
) -> (Vec<String>, HashMap<String, String>) {
    let mut selected_files = Vec::new();
    let mut selected_dirty_files = HashMap::new();
    let initial_attributions = working_log.read_initial_attributions();

    for file_path in files {
        let Some(target_content) = dirty_files.get(&file_path).cloned() else {
            continue;
        };

        let should_replay =
            match working_log.effective_tracked_file_content(&initial_attributions, &file_path) {
                None => true,
                Some(tracked_content) => tracked_content != target_content,
            };

        if should_replay {
            selected_dirty_files.insert(file_path.clone(), target_content);
            selected_files.push(file_path);
        } else {
            debug_log(&format!(
                "Skipping synthetic pre-commit replay for {} because working log already matches committed content",
                file_path
            ));
        }
    }

    (selected_files, selected_dirty_files)
}

fn build_human_replay_agent_result(
    files: Vec<String>,
    dirty_files: HashMap<String, String>,
) -> AgentRunResult {
    AgentRunResult {
        agent_id: crate::authorship::working_log::AgentId {
            tool: "daemon".to_string(),
            id: "daemon-commit-replay".to_string(),
            model: "daemon".to_string(),
        },
        agent_metadata: None,
        transcript: Some(crate::authorship::transcript::AiTranscript { messages: vec![] }),
        checkpoint_kind: CheckpointKind::Human,
        repo_working_dir: None,
        edited_filepaths: None,
        will_edit_filepaths: Some(files),
        dirty_files: Some(dirty_files),
    }
}

fn family_key_for_repository(repo: &Repository) -> String {
    repo.common_dir()
        .canonicalize()
        .unwrap_or_else(|_| repo.common_dir().to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn working_log_has_tracked_state_for_base(repo: &Repository, base_commit: &str) -> bool {
    if !repo.storage.has_working_log(base_commit) {
        return false;
    }

    let working_log = match repo.storage.working_log_for_base_commit(base_commit) {
        Ok(wl) => wl,
        Err(_) => return false,
    };
    let initial = working_log.read_initial_attributions();
    if !initial.files.is_empty() {
        return true;
    }

    working_log
        .read_all_checkpoints()
        .map(|checkpoints| !checkpoints.is_empty())
        .unwrap_or(false)
}

fn capture_recent_working_log_snapshot(
    repo: &Repository,
    base_commit: &str,
    human_author: Option<String>,
) -> Result<Option<RecentWorkingLogSnapshot>, GitAiError> {
    if base_commit.trim().is_empty()
        || base_commit == "initial"
        || !working_log_has_tracked_state_for_base(repo, base_commit)
    {
        return Ok(None);
    }

    let va =
        crate::authorship::virtual_attribution::VirtualAttributions::from_persisted_working_log(
            repo.clone(),
            base_commit.to_string(),
            human_author,
        )?;
    let initial = va.to_initial_working_log_only();
    if initial.files.is_empty() && initial.prompts.is_empty() {
        return Ok(None);
    }

    Ok(Some(RecentWorkingLogSnapshot {
        file_contents: va.snapshot_contents_for_files(initial.files.keys()),
        files: initial.files,
        prompts: initial.prompts,
    }))
}

fn restore_recent_working_log_snapshot(
    repo: &Repository,
    base_commit: &str,
    snapshot: &RecentWorkingLogSnapshot,
) -> Result<bool, GitAiError> {
    if base_commit.trim().is_empty() || snapshot.is_empty() {
        return Ok(false);
    }

    repo.storage
        .working_log_for_base_commit(base_commit)?
        .write_initial_attributions_with_contents(
            snapshot.files.clone(),
            snapshot.prompts.clone(),
            snapshot.file_contents.clone(),
        )?;
    Ok(working_log_has_tracked_state_for_base(repo, base_commit))
}

fn preceding_merge_squash_for_pending_commit(
    repo: &Repository,
    base_commit: &str,
) -> Result<Option<MergeSquashEvent>, GitAiError> {
    let events = repo.storage.read_rewrite_events()?;
    for event in events {
        match event {
            RewriteLogEvent::AuthorshipLogsSynced { .. } => continue,
            RewriteLogEvent::Commit { .. } | RewriteLogEvent::CommitAmend { .. } => continue,
            RewriteLogEvent::MergeSquash { merge_squash }
                if merge_squash.base_head == base_commit =>
            {
                return Ok(Some(merge_squash));
            }
            _ => return Ok(None),
        }
    }
    Ok(None)
}

fn latest_reset_for_base_commit(
    repo: &Repository,
    base_commit: &str,
) -> Result<Option<ResetEvent>, GitAiError> {
    for event in repo.storage.read_rewrite_events()? {
        match event {
            RewriteLogEvent::AuthorshipLogsSynced { .. } => continue,
            RewriteLogEvent::Commit { .. } | RewriteLogEvent::CommitAmend { .. } => continue,
            RewriteLogEvent::Reset { reset }
                if reset.new_head_sha == base_commit
                    && reset.old_head_sha != reset.new_head_sha
                    && !is_zero_oid(&reset.old_head_sha)
                    && !is_zero_oid(&reset.new_head_sha) =>
            {
                return Ok(Some(reset));
            }
            _ => continue,
        }
    }
    Ok(None)
}

fn commit_has_authorship_log(repo: &Repository, commit_sha: &str) -> bool {
    if commit_sha.trim().is_empty()
        || commit_sha == "initial"
        || !is_valid_oid(commit_sha)
        || is_zero_oid(commit_sha)
    {
        return true;
    }

    crate::git::refs::get_reference_as_authorship_log_v3(repo, commit_sha).is_ok()
}

fn rewrite_log_mentions_commit(repo: &Repository, commit_sha: &str) -> Result<bool, GitAiError> {
    if commit_sha.trim().is_empty()
        || commit_sha == "initial"
        || !is_valid_oid(commit_sha)
        || is_zero_oid(commit_sha)
    {
        return Ok(false);
    }

    for event in repo.storage.read_rewrite_events()? {
        let mentioned = match event {
            RewriteLogEvent::Commit { commit } => commit.commit_sha == commit_sha,
            RewriteLogEvent::CommitAmend { commit_amend } => {
                commit_amend.amended_commit_sha == commit_sha
            }
            RewriteLogEvent::RebaseComplete { rebase_complete } => rebase_complete
                .new_commits
                .iter()
                .any(|new_commit| new_commit == commit_sha),
            RewriteLogEvent::CherryPickComplete {
                cherry_pick_complete,
            } => cherry_pick_complete
                .new_commits
                .iter()
                .any(|new_commit| new_commit == commit_sha),
            _ => false,
        };
        if mentioned {
            return Ok(true);
        }
    }

    Ok(false)
}

fn first_parent_commit_chain_exclusive(
    repo: &Repository,
    ancestor_exclusive: Option<&str>,
    head: &str,
) -> Result<Vec<String>, GitAiError> {
    if head.trim().is_empty() || head == "initial" {
        return Ok(Vec::new());
    }

    let stop = ancestor_exclusive
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("initial");
    let mut chain = Vec::new();
    let mut current = head.to_string();

    for _ in 0..512 {
        if current == stop {
            chain.reverse();
            return Ok(chain);
        }

        let commit = repo.find_commit(current.clone())?;
        chain.push(current.clone());

        if commit.parent_count()? == 0 {
            if stop == "initial" {
                chain.reverse();
                return Ok(chain);
            }
            return Err(GitAiError::Generic(format!(
                "commit {} does not reach expected ancestor {} on first-parent chain",
                head, stop
            )));
        }

        current = commit.parent(0)?.id();
    }

    Err(GitAiError::Generic(format!(
        "first-parent chain exceeded limit while walking {} toward {}",
        head, stop
    )))
}

fn materialize_commit_authorship_from_persisted_state_unchecked(
    repo: &Repository,
    commit_sha: &str,
    author: &str,
) -> Result<bool, GitAiError> {
    if commit_has_authorship_log(repo, commit_sha) {
        return Ok(false);
    }

    let parent_sha =
        commit_parent_head_for_capture(repo, commit_sha).unwrap_or_else(|| "initial".to_string());

    let final_state = committed_file_snapshot_between_commits(
        repo,
        if parent_sha == "initial" {
            None
        } else {
            Some(parent_sha.as_str())
        },
        commit_sha,
    )?;

    post_commit_with_final_state(
        repo,
        if parent_sha == "initial" {
            None
        } else {
            Some(parent_sha)
        },
        commit_sha.to_string(),
        author.to_string(),
        true,
        Some(&final_state),
    )?;

    Ok(true)
}

fn materialize_commit_authorship_from_persisted_state(
    repo: &Repository,
    commit_sha: &str,
    author: &str,
) -> Result<bool, GitAiError> {
    if !rewrite_log_mentions_commit(repo, commit_sha)? {
        return Ok(false);
    }

    materialize_commit_authorship_from_persisted_state_unchecked(repo, commit_sha, author)
}

fn attempt_materialize_commit_chain_authorship(
    repo: &Repository,
    ancestor_exclusive: Option<&str>,
    head: &str,
    author: &str,
) -> Result<(), GitAiError> {
    for commit_sha in first_parent_commit_chain_exclusive(repo, ancestor_exclusive, head)? {
        if commit_has_authorship_log(repo, &commit_sha) {
            continue;
        }

        let _ = materialize_commit_authorship_from_persisted_state(repo, &commit_sha, author)?;
    }
    Ok(())
}

fn resolve_reset_old_head_for_base(worktree: &Path, base_commit: &str) -> Option<String> {
    read_ref_oid_for_worktree(worktree, "ORIG_HEAD")
        .filter(|oid| oid != base_commit && is_valid_oid(oid) && !is_zero_oid(oid))
        .or_else(|| {
            resolve_worktree_head_reflog_old_oid_for_new_head(worktree, base_commit)
                .ok()
                .flatten()
                .filter(|oid| oid != base_commit && is_valid_oid(oid) && !is_zero_oid(oid))
        })
}

fn read_reset_recovery_final_state(
    repo: &Repository,
    base_commit: &str,
    old_head: &str,
    user_pathspecs: Option<&[String]>,
    final_state_override: Option<&HashMap<String, String>>,
) -> Result<HashMap<String, String>, GitAiError> {
    if let Some(snapshot) = final_state_override {
        return Ok(snapshot.clone());
    }

    let all_changed_files = repo.diff_changed_files(base_commit, old_head)?;
    let pathspecs: Vec<String> = if let Some(user_paths) = user_pathspecs {
        all_changed_files
            .into_iter()
            .filter(|f| {
                user_paths.iter().any(|p| {
                    f == p
                        || (p.ends_with('/') && f.starts_with(p))
                        || f.starts_with(&format!("{}/", p))
                })
            })
            .collect()
    } else {
        all_changed_files
    };

    let mut final_state = HashMap::new();
    let workdir = repo.workdir()?;
    for file_path in pathspecs {
        let abs_path = workdir.join(&file_path);
        let content = if abs_path.exists() {
            fs::read_to_string(&abs_path).unwrap_or_default()
        } else {
            String::new()
        };
        final_state.insert(file_path, content);
    }

    Ok(final_state)
}

fn restore_matching_old_head_reset_snapshot(
    repo: &Repository,
    base_commit: &str,
    old_head: &str,
    author: &str,
    user_pathspecs: Option<&[String]>,
    final_state_override: Option<&HashMap<String, String>>,
) -> Result<bool, GitAiError> {
    if !repo.storage.has_working_log(old_head) {
        return Ok(false);
    }

    let Some(snapshot) =
        capture_recent_working_log_snapshot(repo, old_head, Some(author.to_string()))?
    else {
        return Ok(false);
    };
    if snapshot.is_empty() {
        return Ok(false);
    }

    let final_state = read_reset_recovery_final_state(
        repo,
        base_commit,
        old_head,
        user_pathspecs,
        final_state_override,
    )?;
    if final_state.is_empty() {
        return Ok(false);
    }

    let matches_current_state = snapshot.file_contents.iter().all(|(file, content)| {
        final_state
            .get(file)
            .is_some_and(|current| current == content)
    });
    if !matches_current_state {
        return Ok(false);
    }

    restore_recent_working_log_snapshot(repo, base_commit, &snapshot)?;
    let _ = repo.storage.delete_working_log_for_base_commit(old_head);
    Ok(true)
}

fn recover_reset_working_log_for_commit_replay(
    repo: &Repository,
    worktree: &Path,
    base_commit: &str,
    author: &str,
    final_state_override: Option<&HashMap<String, String>>,
    pathspecs: Option<&[String]>,
) -> Result<bool, GitAiError> {
    if base_commit.trim().is_empty()
        || base_commit == "initial"
        || working_log_has_tracked_state_for_base(repo, base_commit)
    {
        return Ok(false);
    }

    let old_head = latest_reset_for_base_commit(repo, base_commit)?
        .map(|reset| reset.old_head_sha)
        .or_else(|| resolve_reset_old_head_for_base(worktree, base_commit));
    let Some(old_head) = old_head else {
        return Ok(false);
    };
    if !repo_is_ancestor(repo, base_commit, &old_head) {
        return Ok(false);
    }
    if restore_matching_old_head_reset_snapshot(
        repo,
        base_commit,
        &old_head,
        author,
        pathspecs,
        final_state_override,
    )? {
        return Ok(true);
    }

    if let Err(error) =
        attempt_materialize_commit_chain_authorship(repo, Some(base_commit), &old_head, author)
    {
        debug_log(&format!(
            "Failed to backfill reset prerequisite notes between {} and {}: {}",
            base_commit, old_head, error
        ));
    }
    reconstruct_working_log_after_reset(
        repo,
        base_commit,
        &old_head,
        author,
        pathspecs,
        final_state_override.cloned(),
    )?;
    Ok(true)
}

fn seed_merge_squash_working_log_for_commit_replay(
    repo: &Repository,
    base_commit: &str,
    author: &str,
    exact_final_state: Option<&HashMap<String, String>>,
) -> Result<(), GitAiError> {
    if working_log_has_tracked_state_for_base(repo, base_commit) {
        return Ok(());
    }

    let Some(merge_squash) = preceding_merge_squash_for_pending_commit(repo, base_commit)? else {
        return Ok(());
    };

    let merge_base = repo
        .merge_base(
            merge_squash.source_head.clone(),
            merge_squash.base_head.clone(),
        )
        .ok();
    if let Err(error) = attempt_materialize_commit_chain_authorship(
        repo,
        merge_base.as_deref(),
        &merge_squash.source_head,
        author,
    ) {
        debug_log(&format!(
            "Failed to backfill squash prerequisite notes for {}: {}",
            merge_squash.source_head, error
        ));
    }

    debug_log(&format!(
        "Seeding merge --squash working log before daemon commit replay for base {}",
        base_commit
    ));
    let Some(final_state) = exact_final_state else {
        debug_log(&format!(
            "Skipping merge --squash commit replay seed for {} because no committed final state was available",
            base_commit
        ));
        return Ok(());
    };
    prepare_working_log_after_squash_from_final_state(
        repo,
        &merge_squash.source_head,
        base_commit,
        final_state,
        author,
    )
}

fn recover_recent_replay_prerequisites_for_commit_replay(
    coordinator: &ActorDaemonCoordinator,
    repo: &Repository,
    base_commit: &str,
    author: &str,
) -> Result<(), GitAiError> {
    if base_commit.trim().is_empty() || base_commit == "initial" {
        return Ok(());
    }

    let family = family_key_for_repository(repo);
    for prerequisite in coordinator.recent_replay_prerequisites_for_base(&family, base_commit)? {
        match prerequisite {
            RecentReplayPrerequisite::Reset {
                target_head,
                old_head,
                pathspecs,
                final_state,
                working_log_snapshot,
            } => {
                if target_head != base_commit || old_head.is_empty() {
                    continue;
                }
                if old_head == base_commit && !pathspecs.is_empty() {
                    remove_working_log_attributions_for_pathspecs(repo, base_commit, &pathspecs)?;
                    return Ok(());
                }
                if working_log_has_tracked_state_for_base(repo, base_commit) {
                    continue;
                }
                if let Some(snapshot) = working_log_snapshot.as_ref()
                    && restore_recent_working_log_snapshot(repo, base_commit, snapshot)?
                {
                    return Ok(());
                }
                if let Err(error) = attempt_materialize_commit_chain_authorship(
                    repo,
                    Some(base_commit),
                    &old_head,
                    author,
                ) {
                    debug_log(&format!(
                        "Failed to backfill recent reset prerequisite notes between {} and {}: {}",
                        base_commit, old_head, error
                    ));
                }
                reconstruct_working_log_after_reset(
                    repo,
                    base_commit,
                    &old_head,
                    author,
                    if pathspecs.is_empty() {
                        None
                    } else {
                        Some(pathspecs.as_slice())
                    },
                    final_state,
                )?;
            }
            RecentReplayPrerequisite::CheckoutSwitchRename {
                target_head,
                old_head,
            } => {
                if working_log_has_tracked_state_for_base(repo, base_commit) {
                    continue;
                }
                if target_head != base_commit
                    || old_head.is_empty()
                    || !repo.storage.has_working_log(&old_head)
                {
                    continue;
                }
                repo.storage.rename_working_log(&old_head, base_commit)?;
            }
            RecentReplayPrerequisite::CheckoutSwitchMerge {
                target_head,
                old_head,
                final_state,
            } => {
                if working_log_has_tracked_state_for_base(repo, base_commit) {
                    continue;
                }
                if target_head != base_commit
                    || old_head.is_empty()
                    || final_state.is_empty()
                    || !repo.storage.has_working_log(&old_head)
                {
                    continue;
                }
                restore_working_log_carryover(
                    repo,
                    &old_head,
                    base_commit,
                    final_state,
                    Some(author.to_string()),
                )?;
                let _ = repo.storage.delete_working_log_for_base_commit(&old_head);
            }
            RecentReplayPrerequisite::StashRestore {
                target_head,
                stash_sha,
            } => {
                if working_log_has_tracked_state_for_base(repo, base_commit) {
                    continue;
                }
                if target_head != base_commit || stash_sha.is_empty() {
                    continue;
                }
                stash_hooks::restore_stash_attributions(repo, base_commit, &stash_sha)?;
            }
        }

        if working_log_has_tracked_state_for_base(repo, base_commit) {
            return Ok(());
        }
    }

    Ok(())
}

fn ensure_rewrite_prerequisites(
    coordinator: &ActorDaemonCoordinator,
    repo: &Repository,
    worktree: &Path,
    rewrite_event: &RewriteLogEvent,
    author: &str,
    carryover_snapshot: Option<&HashMap<String, String>>,
    reset_pathspecs: Option<&[String]>,
) -> Result<(), GitAiError> {
    let Some((base_commit, _target_commit)) =
        commit_replay_context_from_rewrite_event(rewrite_event)
    else {
        return Ok(());
    };
    if base_commit.trim().is_empty() {
        return Ok(());
    }

    if base_commit != "initial" && matches!(rewrite_event, RewriteLogEvent::CommitAmend { .. }) {
        let materialize_result = materialize_commit_authorship_from_persisted_state_unchecked(
            repo,
            &base_commit,
            author,
        )
        .map(|_| ());
        if let Err(error) = materialize_result {
            debug_log(&format!(
                "Failed to backfill base commit note for {}: {}",
                base_commit, error
            ));
        }
    }

    let exact_final_state =
        exact_final_state_for_commit_replay(repo, rewrite_event, carryover_snapshot)?;
    recover_recent_replay_prerequisites_for_commit_replay(coordinator, repo, &base_commit, author)?;
    seed_merge_squash_working_log_for_commit_replay(
        repo,
        &base_commit,
        author,
        exact_final_state.as_ref(),
    )?;
    if working_log_has_tracked_state_for_base(repo, &base_commit) {
        return Ok(());
    }

    recover_reset_working_log_for_commit_replay(
        repo,
        worktree,
        &base_commit,
        author,
        exact_final_state.as_ref(),
        reset_pathspecs,
    )?;

    Ok(())
}

fn sync_pre_commit_checkpoint_for_daemon_commit(
    repo: &Repository,
    rewrite_event: &RewriteLogEvent,
    author: &str,
    carryover_snapshot: Option<&HashMap<String, String>>,
) -> Result<(), GitAiError> {
    let Some((base_commit, target_commit)) =
        commit_replay_context_from_rewrite_event(rewrite_event)
    else {
        return Ok(());
    };
    if base_commit.trim().is_empty() || target_commit.trim().is_empty() {
        return Ok(());
    }
    let dirty_files = if let Some(snapshot) = carryover_snapshot {
        snapshot.clone()
    } else {
        committed_file_snapshot_between_commits(
            repo,
            if base_commit == "initial" {
                None
            } else {
                Some(base_commit.as_str())
            },
            &target_commit,
        )?
    };
    let changed_files = commit_replay_files_from_snapshot(&dirty_files);
    if changed_files.is_empty() {
        return Ok(());
    }
    let working_log = repo.storage.working_log_for_base_commit(&base_commit)?;
    let (changed_files, dirty_files) =
        filter_commit_replay_files(&working_log, changed_files, dirty_files);
    if changed_files.is_empty() {
        return Ok(());
    }
    let replay_agent_result = build_human_replay_agent_result(changed_files, dirty_files);

    crate::commands::checkpoint::run_with_base_commit_override_with_policy(
        repo,
        author,
        CheckpointKind::Human,
        false,
        true,
        Some(replay_agent_result),
        base_commit != "initial",
        Some(base_commit.as_str()),
        crate::commands::checkpoint::BaseOverrideResolutionPolicy::RequireExplicitSnapshot,
    )
    .map(|_| ())
}

fn apply_rewrite_side_effect(
    coordinator: &ActorDaemonCoordinator,
    family: Option<&str>,
    worktree: &str,
    rewrite_event: RewriteLogEvent,
    carryover_snapshot: Option<&HashMap<String, String>>,
    reset_pathspecs: Option<&[String]>,
) -> Result<(), GitAiError> {
    let mut repo = find_repository_in_path(worktree)?;
    let author = repo.git_author_identity().name_or_unknown();
    ensure_rewrite_prerequisites(
        coordinator,
        &repo,
        Path::new(worktree),
        &rewrite_event,
        &author,
        carryover_snapshot,
        reset_pathspecs,
    )?;
    let prerequisite_family = family
        .map(std::borrow::ToOwned::to_owned)
        .unwrap_or_else(|| family_key_for_repository(&repo));
    if let RewriteLogEvent::Reset { reset } = &rewrite_event {
        apply_reset_working_log_side_effect(
            &repo,
            reset,
            &author,
            carryover_snapshot,
            reset_pathspecs,
        )?;
        coordinator.record_recent_replay_prerequisite(
            &prerequisite_family,
            RecentReplayPrerequisite::Reset {
                target_head: reset.new_head_sha.clone(),
                old_head: reset.old_head_sha.clone(),
                pathspecs: reset_pathspecs
                    .map(|paths| paths.to_vec())
                    .unwrap_or_default(),
                final_state: carryover_snapshot.cloned(),
                working_log_snapshot: capture_recent_working_log_snapshot(
                    &repo,
                    &reset.new_head_sha,
                    Some(author.clone()),
                )?,
            },
        )?;
    }
    if !rewrite_event_needs_authorship_processing(&repo, &rewrite_event)? {
        let _ = repo.storage.append_rewrite_event(rewrite_event);
        return Ok(());
    }
    match &rewrite_event {
        RewriteLogEvent::Stash { stash }
            if matches!(stash.operation, StashOperation::Apply | StashOperation::Pop) =>
        {
            if let (Some(head_sha), Some(stash_sha)) =
                (stash.head_sha.as_ref(), stash.stash_sha.as_ref())
            {
                coordinator.record_recent_replay_prerequisite(
                    &prerequisite_family,
                    RecentReplayPrerequisite::StashRestore {
                        target_head: head_sha.clone(),
                        stash_sha: stash_sha.clone(),
                    },
                )?;
            }
        }
        _ => {}
    }
    if let RewriteLogEvent::Stash { stash } = &rewrite_event {
        apply_stash_rewrite_side_effect(&mut repo, stash)?;
    }
    let committed_final_state = stable_final_state_for_commit_rewrite(&repo, &rewrite_event)?;
    let normalized_carryover_snapshot =
        normalize_commit_carryover_snapshot(carryover_snapshot, committed_final_state.as_ref());
    let normalized_carryover_snapshot_ref = normalized_carryover_snapshot.as_ref();
    let deferred_commit_carryover = deferred_commit_carryover_context(
        &repo,
        &rewrite_event,
        &author,
        normalized_carryover_snapshot_ref,
    )?;
    sync_pre_commit_checkpoint_for_daemon_commit(
        &repo,
        &rewrite_event,
        &author,
        normalized_carryover_snapshot_ref,
    )?;
    let log = repo.storage.append_rewrite_event(rewrite_event.clone())?;
    match &rewrite_event {
        RewriteLogEvent::Commit { commit } => {
            let final_state_override =
                normalized_carryover_snapshot_ref.or(committed_final_state.as_ref());
            post_commit_with_final_state(
                &repo,
                commit.base_commit.clone(),
                commit.commit_sha.clone(),
                author.clone(),
                true,
                final_state_override,
            )?;
        }
        RewriteLogEvent::CommitAmend { commit_amend } => {
            let final_state_override =
                normalized_carryover_snapshot_ref.or(committed_final_state.as_ref());
            rewrite_authorship_after_commit_amend_with_snapshot(
                &repo,
                &commit_amend.original_commit,
                &commit_amend.amended_commit_sha,
                author.clone(),
                final_state_override,
            )?;
        }
        _ => {
            rewrite_authorship_if_needed(&repo, &rewrite_event, author.clone(), &log, true)?;
        }
    }
    if let Some((target_commit, carried_va, final_state)) = deferred_commit_carryover {
        restore_virtual_attribution_carryover(&repo, &target_commit, carried_va, final_state)?;
    }
    if let Some(family) = family
        && let Some((base_commit, _)) = commit_replay_context_from_rewrite_event(&rewrite_event)
        && !base_commit.trim().is_empty()
    {
        coordinator.discard_recent_replay_prerequisites_for_base(family, &base_commit)?;
    }
    Ok(())
}

fn rewrite_event_needs_authorship_processing(
    repo: &Repository,
    rewrite_event: &RewriteLogEvent,
) -> Result<bool, GitAiError> {
    // Full wrapper parity requires authorship notes for every commit, even when the commit is
    // entirely human-authored.
    if matches!(
        rewrite_event,
        RewriteLogEvent::Commit { .. } | RewriteLogEvent::CommitAmend { .. }
    ) {
        return Ok(true);
    }

    let Some((base_commit, _)) = commit_replay_context_from_rewrite_event(rewrite_event) else {
        return Ok(true);
    };
    let working_log = repo.storage.working_log_for_base_commit(&base_commit)?;
    let has_initial = !working_log.read_initial_attributions().files.is_empty();
    if has_initial {
        return Ok(true);
    }
    let has_ai_checkpoints = working_log
        .read_all_checkpoints()?
        .iter()
        .any(|checkpoint| checkpoint.kind != CheckpointKind::Human);
    Ok(has_ai_checkpoints)
}

fn deferred_commit_carryover_context(
    repo: &Repository,
    rewrite_event: &RewriteLogEvent,
    author: &str,
    carryover_snapshot: Option<&HashMap<String, String>>,
) -> Result<Option<DeferredCommitCarryover>, GitAiError> {
    let Some(snapshot) = carryover_snapshot else {
        return Ok(None);
    };
    let Some((base_commit, target_commit)) =
        commit_replay_context_from_rewrite_event(rewrite_event)
    else {
        return Ok(None);
    };
    let committed_snapshot = committed_file_snapshot_between_commits(
        repo,
        if base_commit == "initial" {
            None
        } else {
            Some(base_commit.as_str())
        },
        &target_commit,
    )?;
    let remaining_state = snapshot
        .iter()
        .filter_map(|(file, content)| {
            if committed_snapshot
                .get(file)
                .is_some_and(|committed| committed == content)
            {
                None
            } else {
                Some((file.clone(), content.clone()))
            }
        })
        .collect::<HashMap<_, _>>();
    if base_commit.trim().is_empty()
        || target_commit.trim().is_empty()
        || remaining_state.is_empty()
        || !working_log_has_tracked_state_for_base(repo, &base_commit)
    {
        return Ok(None);
    }

    let carried_va =
        crate::authorship::virtual_attribution::VirtualAttributions::from_persisted_working_log(
            repo.clone(),
            base_commit,
            Some(author.to_string()),
        )?;
    if carried_va.attributions.is_empty() {
        return Ok(None);
    }

    Ok(Some((target_commit, carried_va, remaining_state)))
}

fn apply_stash_rewrite_side_effect(
    repo: &mut Repository,
    stash_event: &StashEvent,
) -> Result<(), GitAiError> {
    match stash_event.operation {
        StashOperation::Create => {
            let Some(head_sha) = stash_event.head_sha.as_deref() else {
                return Err(GitAiError::Generic(
                    "stash create missing destination head".to_string(),
                ));
            };
            let Some(stash_sha) = stash_event.stash_sha.as_deref() else {
                debug_log("Skipping stash create replay without created stash oid");
                return Ok(());
            };
            stash_hooks::save_stash_authorship_log(
                repo,
                head_sha,
                stash_sha,
                &stash_event.pathspecs,
            )?;
        }
        StashOperation::Apply | StashOperation::Pop => {
            let Some(head_sha) = stash_event.head_sha.as_deref() else {
                return Err(GitAiError::Generic(
                    "stash apply/pop missing destination head".to_string(),
                ));
            };
            let Some(stash_sha) = stash_event.stash_sha.as_deref() else {
                return Err(GitAiError::Generic(
                    "stash apply/pop missing stash oid".to_string(),
                ));
            };
            stash_hooks::restore_stash_attributions(repo, head_sha, stash_sha)?;
        }
        StashOperation::Drop | StashOperation::List => {}
    }
    Ok(())
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

type RebaseCommitMappings = (Vec<String>, Vec<String>);

fn processed_rebase_new_heads(repository: &Repository) -> Result<HashSet<String>, GitAiError> {
    let mut out = HashSet::new();
    for event in repository.storage.read_rewrite_events()? {
        if let RewriteLogEvent::RebaseComplete { rebase_complete } = event {
            out.insert(rebase_complete.new_head);
        }
    }
    Ok(out)
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

fn maybe_rebase_mappings_from_repository(
    repository: &Repository,
    old_head: &str,
    new_head: &str,
    onto_head: Option<&str>,
    context: &str,
) -> Result<Option<RebaseCommitMappings>, GitAiError> {
    let (original_commits, new_commits) =
        crate::commands::hooks::rebase_hooks::build_rebase_commit_mappings(
            repository, old_head, new_head, onto_head,
        )?;
    if original_commits.is_empty() {
        debug_log(&format!(
            "{} produced no rebase source commits; skipping rewrite synthesis",
            context
        ));
        return Ok(None);
    }
    if new_commits.is_empty() {
        debug_log(&format!(
            "{} produced no rebased commits; skipping rewrite synthesis",
            context
        ));
        return Ok(None);
    }
    Ok(Some((original_commits, new_commits)))
}

fn strict_cherry_pick_mappings_from_command(
    cmd: &crate::daemon::domain::NormalizedCommand,
    new_head: &str,
    pending_source_commits: Vec<String>,
    context: &str,
) -> Result<(String, Vec<String>, Vec<String>), GitAiError> {
    if new_head.is_empty() {
        return Err(GitAiError::Generic(format!(
            "{} invalid cherry-pick new head new={}",
            context, new_head
        )));
    }
    let worktree = cmd.worktree.as_deref().ok_or_else(|| {
        GitAiError::Generic(format!(
            "{} missing worktree for cherry-pick mapping new={}",
            context, new_head
        ))
    })?;
    // Resolve source commits: prefer pending (cached from start event), fall
    // back to parsing command args.  Either path may contain short SHAs or
    // symbolic refs, so resolve them to full OIDs via git rev-parse.  This
    // runs in the async side-effect path, not the daemon critical path.
    let mut source_refs = pending_source_commits;
    if source_refs.is_empty() {
        source_refs = cherry_pick_source_refs_from_command(cmd);
    }
    if source_refs.is_empty() {
        return Err(GitAiError::Generic(format!(
            "{} missing cherry-pick source commits",
            context
        )));
    }
    let source_commits = resolve_cherry_pick_source_refs(&source_refs, worktree, context)?;
    if source_commits.is_empty() {
        return Err(GitAiError::Generic(format!(
            "{} cherry-pick source refs resolved to no valid commits",
            context
        )));
    }
    let (original_head, new_commits) = resolve_linear_head_commit_chain_for_worktree(
        worktree,
        new_head,
        source_commits.len(),
        Some("cherry-pick"),
    )
    .map_err(|err| {
        GitAiError::Generic(format!(
            "{} failed to reconstruct cherry-pick commits new={} expected_count={}: {}",
            context,
            new_head,
            source_commits.len(),
            err
        ))
    })?;
    Ok((original_head, source_commits, new_commits))
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

/// Resolve cherry-pick source refs (which may be short SHAs, branch names, or
/// full OIDs) to full commit OIDs. This calls `git rev-parse` and MUST only be
/// invoked from the async side-effect path, never the daemon critical path.
fn resolve_cherry_pick_source_refs(
    source_refs: &[String],
    worktree: &Path,
    context: &str,
) -> Result<Vec<String>, GitAiError> {
    let mut resolved = Vec::new();
    let repo = find_repository_in_path(worktree.to_string_lossy().as_ref())?;
    for src in source_refs {
        if is_valid_oid(src) && !is_zero_oid(src) {
            resolved.push(src.clone());
        } else {
            let obj = repo.revparse_single(src).map_err(|err| {
                GitAiError::Generic(format!(
                    "{} failed to resolve cherry-pick source ref '{}': {}",
                    context, src, err
                ))
            })?;
            let oid = obj
                .peel_to_commit()
                .map(|c| c.id())
                .unwrap_or_else(|_| obj.id());
            if is_valid_oid(&oid) && !is_zero_oid(&oid) {
                resolved.push(oid);
            }
        }
    }
    Ok(resolved)
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

fn rebase_start_target_hint_from_command(
    cmd: &crate::daemon::domain::NormalizedCommand,
) -> Option<String> {
    rebase_start_target_hint_from_args(&cmd.invoked_args)
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

fn repository_for_rewrite_context(
    cmd: &crate::daemon::domain::NormalizedCommand,
    context: &str,
) -> Result<Repository, GitAiError> {
    if let Some(worktree) = cmd.worktree.as_ref()
        && let Ok(repository) = find_repository_in_path(&worktree.to_string_lossy())
    {
        return Ok(repository);
    }
    Err(GitAiError::Generic(format!(
        "{} requires repository context from command worktree",
        context,
    )))
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

fn apply_reset_working_log_side_effect(
    repository: &crate::git::repository::Repository,
    reset: &ResetEvent,
    human_author: &str,
    carryover_snapshot: Option<&HashMap<String, String>>,
    pathspecs: Option<&[String]>,
) -> Result<(), GitAiError> {
    if reset.old_head_sha.is_empty()
        || reset.new_head_sha.is_empty()
        || is_zero_oid(&reset.old_head_sha)
        || is_zero_oid(&reset.new_head_sha)
    {
        return Ok(());
    }

    if reset.kind == ResetKind::Hard {
        let _ = repository
            .storage
            .delete_working_log_for_base_commit(&reset.old_head_sha);
        return Ok(());
    }

    if reset.old_head_sha == reset.new_head_sha && pathspecs.is_none_or(|paths| paths.is_empty()) {
        return Ok(());
    }

    if reset.old_head_sha == reset.new_head_sha {
        if let Some(pathspecs) = pathspecs.filter(|paths| !paths.is_empty()) {
            remove_working_log_attributions_for_pathspecs(
                repository,
                &reset.old_head_sha,
                pathspecs,
            )?;
        }
        return Ok(());
    }

    let is_backward = repo_is_ancestor(repository, &reset.new_head_sha, &reset.old_head_sha);
    if is_backward {
        if let Err(error) = attempt_materialize_commit_chain_authorship(
            repository,
            Some(&reset.new_head_sha),
            &reset.old_head_sha,
            human_author,
        ) {
            debug_log(&format!(
                "Failed to backfill reset-side-effect notes between {} and {}: {}",
                reset.new_head_sha, reset.old_head_sha, error
            ));
        }
        let tracked_files = tracked_working_log_files(repository, &reset.old_head_sha)?;
        if !tracked_files.is_empty() && carryover_snapshot.is_none() {
            return Err(GitAiError::Generic(format!(
                "reset {} -> {} missing captured carryover snapshot",
                reset.old_head_sha, reset.new_head_sha
            )));
        }
        reconstruct_working_log_after_reset(
            repository,
            &reset.new_head_sha,
            &reset.old_head_sha,
            human_author,
            pathspecs,
            carryover_snapshot.cloned(),
        )?;
    } else {
        let _ = repository
            .storage
            .delete_working_log_for_base_commit(&reset.old_head_sha);
    }
    Ok(())
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

#[cfg(unix)]
fn daemon_is_test_mode() -> bool {
    std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
        || std::env::var_os("GITAI_TEST_DB_PATH").is_some()
}

#[cfg(unix)]
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
            debug_log(&format!("daemon log file setup failed: {}", e));
            None
        }
    }
}

#[cfg(windows)]
fn maybe_setup_daemon_log_file(_config: &DaemonConfig) -> Option<DaemonLogGuard> {
    None
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

/// Remove log files from previous daemon runs that are older than one week and
/// whose PID is no longer alive, to avoid unbounded growth while keeping recent
/// logs available for debugging.
#[cfg(unix)]
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
        let pid: u32 = match stem.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if process_alive(pid) {
            continue;
        }
        let dominated = path
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > one_week);
        if dominated {
            let _ = fs::remove_file(&path);
        }
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
        request: Box<CheckpointRunRequest>,
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

#[derive(Debug, Clone, Default)]
struct RecentWorkingLogSnapshot {
    files: HashMap<String, Vec<crate::authorship::attribution_tracker::LineAttribution>>,
    prompts: HashMap<String, crate::authorship::authorship_log::PromptRecord>,
    file_contents: HashMap<String, String>,
}

impl RecentWorkingLogSnapshot {
    fn is_empty(&self) -> bool {
        self.files.is_empty() && self.prompts.is_empty()
    }
}

#[derive(Debug, Clone)]
enum RecentReplayPrerequisite {
    Reset {
        target_head: String,
        old_head: String,
        pathspecs: Vec<String>,
        final_state: Option<HashMap<String, String>>,
        working_log_snapshot: Option<RecentWorkingLogSnapshot>,
    },
    CheckoutSwitchRename {
        target_head: String,
        old_head: String,
    },
    CheckoutSwitchMerge {
        target_head: String,
        old_head: String,
        final_state: HashMap<String, String>,
    },
    StashRestore {
        target_head: String,
        stash_sha: String,
    },
}

impl RecentReplayPrerequisite {
    fn target_head(&self) -> &str {
        match self {
            Self::Reset { target_head, .. }
            | Self::CheckoutSwitchRename { target_head, .. }
            | Self::CheckoutSwitchMerge { target_head, .. }
            | Self::StashRestore { target_head, .. } => target_head,
        }
    }
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

struct ActorDaemonCoordinator {
    backend: Arc<crate::daemon::git_backend::SystemGitBackend>,
    coordinator:
        Arc<crate::daemon::coordinator::Coordinator<crate::daemon::git_backend::SystemGitBackend>>,
    normalizer: AsyncMutex<
        crate::daemon::trace_normalizer::TraceNormalizer<
            crate::daemon::git_backend::SystemGitBackend,
        >,
    >,
    pending_rebase_original_head_by_worktree: Mutex<HashMap<String, String>>,
    pending_cherry_pick_sources_by_worktree: Mutex<HashMap<String, Vec<String>>>,
    inflight_effects_by_family: Mutex<HashMap<String, usize>>,
    family_sequencers_by_family: Mutex<HashMap<String, FamilySequencerState>>,
    pending_root_slots_by_root: Mutex<HashMap<String, PendingRootSlot>>,
    recent_replay_prerequisites_by_family:
        Mutex<HashMap<String, VecDeque<RecentReplayPrerequisite>>>,
    side_effect_errors_by_family: Mutex<HashMap<String, BTreeMap<u64, String>>>,
    side_effect_exec_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    carryover_snapshots_by_id: Mutex<HashMap<String, HashMap<String, String>>>,
    carryover_snapshot_ids_by_root: Mutex<HashMap<String, Vec<String>>>,
    test_completion_log_dir: Option<PathBuf>,
    test_completion_log_lock: Mutex<()>,
    trace_ingest_tx: Mutex<Option<mpsc::Sender<Value>>>,
    telemetry_worker: Option<crate::daemon::telemetry_worker::DaemonTelemetryWorkerHandle>,
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
    shutdown_notify: Notify,
    shutdown_condvar: std::sync::Condvar,
    shutdown_condvar_mutex: Mutex<()>,
}

struct WrapperStateEntry {
    pre_repo: Option<RepoContext>,
    post_repo: Option<RepoContext>,
    received_at_ns: u128,
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
            family_sequencers_by_family: Mutex::new(HashMap::new()),
            pending_root_slots_by_root: Mutex::new(HashMap::new()),
            recent_replay_prerequisites_by_family: Mutex::new(HashMap::new()),
            side_effect_errors_by_family: Mutex::new(HashMap::new()),
            side_effect_exec_locks: Mutex::new(HashMap::new()),
            carryover_snapshots_by_id: Mutex::new(HashMap::new()),
            carryover_snapshot_ids_by_root: Mutex::new(HashMap::new()),
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
            trace_ingest_tx: Mutex::new(None),
            telemetry_worker: None,
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
            shutdown_notify: Notify::new(),
            shutdown_condvar: std::sync::Condvar::new(),
            shutdown_condvar_mutex: Mutex::new(()),
        }
    }

    fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    fn request_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        if let Ok(mut tx) = self.trace_ingest_tx.lock() {
            let _ = tx.take();
        }
        self.shutdown_notify.notify_waiters();
        // Hold the condvar mutex so notify_all cannot race with the
        // check-then-wait sequence in daemon_update_check_loop.
        let _guard = self
            .shutdown_condvar_mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.shutdown_condvar.notify_all();
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

    fn recent_replay_prerequisites_for_base(
        &self,
        family: &str,
        base_commit: &str,
    ) -> Result<Vec<RecentReplayPrerequisite>, GitAiError> {
        let map = self
            .recent_replay_prerequisites_by_family
            .lock()
            .map_err(|_| {
                GitAiError::Generic("recent replay prerequisites map lock poisoned".to_string())
            })?;
        let matches: Vec<RecentReplayPrerequisite> = map
            .get(family)
            .map(|entries| {
                entries
                    .iter()
                    .rev()
                    .filter(|entry| entry.target_head() == base_commit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        Ok(matches)
    }

    fn discard_recent_replay_prerequisites_for_base(
        &self,
        family: &str,
        base_commit: &str,
    ) -> Result<(), GitAiError> {
        let mut map = self
            .recent_replay_prerequisites_by_family
            .lock()
            .map_err(|_| {
                GitAiError::Generic("recent replay prerequisites map lock poisoned".to_string())
            })?;
        if let Some(entries) = map.get_mut(family) {
            entries.retain(|entry| entry.target_head() != base_commit);
            if entries.is_empty() {
                map.remove(family);
            }
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
            debug_log(&format!(
                "daemon trace connection close fallback finalized sid={}",
                root_sid
            ));
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
            self.next_carryover_snapshot_id
                .fetch_add(1, Ordering::SeqCst)
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
        if input.exit_code != 0 {
            return Ok(None);
        }

        let parsed = parse_git_cli_args(trace_invocation_args(input.argv));
        let command = parsed.command.as_deref().or(input.primary_command);
        let Some(command) = command else {
            return Ok(None);
        };

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
        (self.next_trace_ingest_seq.fetch_add(1, Ordering::SeqCst) as u64) + 1
    }

    fn trace_ingest_high_watermark(&self) -> u64 {
        self.next_trace_ingest_seq.load(Ordering::SeqCst) as u64
    }

    async fn wait_for_trace_ingest_processed_through(&self, seq: u64) -> Result<(), GitAiError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let notified = self.trace_ingest_progress_notify.notified();
            let processed = self.processed_trace_ingest_seq.load(Ordering::SeqCst) as u64;
            if processed >= seq {
                return Ok(());
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(GitAiError::Generic(format!(
                    "timed out waiting for trace ingest through seq {} (processed={})",
                    seq, processed
                )));
            }
            let wait_for = deadline.saturating_duration_since(now);
            let _ = tokio::time::timeout(wait_for, notified).await;
        }
    }

    fn start_trace_ingest_worker(self: &Arc<Self>) -> Result<(), GitAiError> {
        let mut guard = self
            .trace_ingest_tx
            .lock()
            .map_err(|_| GitAiError::Generic("trace ingest tx lock poisoned".to_string()))?;
        if guard.is_some() {
            return Ok(());
        }

        const TRACE_INGEST_QUEUE_CAPACITY: usize = 16_384;
        let (tx, mut rx) = mpsc::channel::<Value>(TRACE_INGEST_QUEUE_CAPACITY);
        *guard = Some(tx);
        drop(guard);

        let coordinator = self.clone();
        tokio::spawn(async move {
            let mut next_seq: u64 = 1;
            let mut pending_by_seq: BTreeMap<u64, Value> = BTreeMap::new();
            let mut gc_counter: u64 = 0;
            const GC_INTERVAL: u64 = 500;

            while let Some(payload) = rx.recv().await {
                let Some(seq) = payload.get(TRACE_INGEST_SEQ_FIELD).and_then(Value::as_u64) else {
                    let error = GitAiError::Generic(
                        "trace ingest payload missing ingress sequence".to_string(),
                    );
                    observability::log_error(
                        &error,
                        Some(serde_json::json!({
                            "component": "daemon",
                            "phase": "trace_ingest_worker",
                            "reason": "missing_ingest_seq",
                            "payload": payload,
                        })),
                    );
                    coordinator.request_shutdown();
                    break;
                };

                if pending_by_seq.len() >= TRACE_INGEST_QUEUE_CAPACITY {
                    let error = GitAiError::Generic(format!(
                        "trace ingest reorder buffer overflow at {} entries; next_seq={}",
                        pending_by_seq.len(),
                        next_seq
                    ));
                    debug_log(&format!("{}", error));
                    observability::log_error(
                        &error,
                        Some(serde_json::json!({
                            "component": "daemon",
                            "phase": "trace_ingest_worker",
                            "reason": "reorder_buffer_overflow",
                            "buffered_count": pending_by_seq.len(),
                            "next_seq": next_seq,
                            "received_seq": seq,
                        })),
                    );
                    coordinator.request_shutdown();
                    break;
                }

                if pending_by_seq.insert(seq, payload).is_some() {
                    let error = GitAiError::Generic(format!(
                        "duplicate trace ingest sequence received: {}",
                        seq
                    ));
                    observability::log_error(
                        &error,
                        Some(serde_json::json!({
                            "component": "daemon",
                            "phase": "trace_ingest_worker",
                            "reason": "duplicate_ingest_seq",
                            "sequence": seq,
                        })),
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
                                debug_log(&format!("daemon trace ingest error: {}", error));
                                observability::log_error(
                                    &error,
                                    Some(serde_json::json!({
                                        "component": "daemon",
                                        "phase": "trace_ingest_worker",
                                        "reason": "ingest_error",
                                        "sequence": processed_seq,
                                        "root_sid": ordered_payload_root,
                                    })),
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
                                let error = GitAiError::Generic(format!(
                                    "trace ingest worker panic: {}",
                                    panic_msg
                                ));
                                debug_log(&format!("daemon trace ingest panic: {}", panic_msg));
                                observability::log_error(
                                    &error,
                                    Some(serde_json::json!({
                                        "component": "daemon",
                                        "phase": "trace_ingest_worker",
                                        "reason": "panic_in_ingest",
                                        "panic_message": panic_msg,
                                        "sequence": processed_seq,
                                    })),
                                );
                                Err(error)
                            }
                        }
                    };
                    let _ = ingest_result;
                    let _ = coordinator.queued_trace_payloads.fetch_update(
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                        |current| Some(current.saturating_sub(1)),
                    );
                    if let Err(error) = coordinator
                        .record_trace_payload_processed_root(ordered_payload_root.as_deref())
                    {
                        debug_log(&format!(
                            "daemon trace payload accounting error after ingest: {}",
                            error
                        ));
                    }
                    coordinator
                        .processed_trace_ingest_seq
                        .store(processed_seq as usize, Ordering::SeqCst);
                    coordinator.trace_ingest_progress_notify.notify_waiters();
                    next_seq = next_seq.saturating_add(1);
                    gc_counter += 1;
                    if gc_counter.is_multiple_of(GC_INTERVAL) {
                        coordinator.gc_stale_family_state();
                    }
                }
            }

            if !pending_by_seq.is_empty() {
                let error = GitAiError::Generic(format!(
                    "trace ingest worker exiting with {} buffered out-of-order frame(s); next_seq={}",
                    pending_by_seq.len(),
                    next_seq
                ));
                observability::log_error(
                    &error,
                    Some(serde_json::json!({
                        "component": "daemon",
                        "phase": "trace_ingest_worker",
                        "reason": "unflushed_buffer_on_shutdown",
                        "buffered_count": pending_by_seq.len(),
                        "next_seq": next_seq,
                        "min_buffered_seq": pending_by_seq.keys().next().copied(),
                        "max_buffered_seq": pending_by_seq.keys().last().copied(),
                    })),
                );
            }
        });
        Ok(())
    }

    fn enqueue_trace_payload(&self, payload: Value) -> Result<(), GitAiError> {
        let tx = self
            .trace_ingest_tx
            .lock()
            .map_err(|_| GitAiError::Generic("trace ingest tx lock poisoned".to_string()))?
            .as_ref()
            .cloned()
            .ok_or_else(|| GitAiError::Generic("trace ingest worker not started".to_string()))?;
        let payload_root = Self::trace_payload_root_sid(&payload);
        self.record_trace_payload_enqueued(&payload)?;
        self.queued_trace_payloads.fetch_add(1, Ordering::SeqCst);
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
                Ordering::SeqCst,
                Ordering::SeqCst,
                |current| Some(current.saturating_sub(1)),
            );
            if let Err(error) = self.record_trace_payload_processed_root(payload_root.as_deref()) {
                debug_log(&format!(
                    "daemon trace payload accounting rollback error: {}",
                    error
                ));
            }
            let error = GitAiError::Generic(
                "trace ingest queue send failed: worker may have crashed".to_string(),
            );
            observability::log_error(
                &error,
                Some(serde_json::json!({
                    "component": "daemon",
                    "phase": "enqueue_trace_payload",
                    "reason": "ingest_worker_channel_closed",
                })),
            );
            self.request_shutdown();
            return Err(error);
        }
        Ok(())
    }

    fn prepare_trace_payload_for_ingest(&self, payload: &mut Value) {
        if let Some(object) = payload.as_object_mut()
            && object.get(TRACE_INGEST_SEQ_FIELD).is_none()
        {
            object.insert(
                TRACE_INGEST_SEQ_FIELD.to_string(),
                json!(self.next_trace_ingest_seq()),
            );
        }
        self.augment_trace_payload_with_reflog_metadata(payload);
    }

    fn augment_trace_payload_with_reflog_metadata(&self, payload: &mut Value) {
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
            return;
        }

        let root = trace_root_sid(&sid).to_string();

        // Fast path: for commands that are definitively read-only (check-ignore,
        // rev-parse, status, etc.), skip all expensive filesystem I/O (worktree
        // resolution, HEAD state reads, reflog captures) and do only lightweight
        // bookkeeping. This dramatically reduces CPU when IDEs fire many read-only
        // git commands per second.
        let early_primary = trace_payload_primary_command(payload).or_else(|| {
            let argv = trace_payload_argv(payload);
            trace_argv_primary_command(&argv)
        });
        // Check if this specific event's command is read-only, OR if this root
        // was already identified as read-only from a prior event (e.g. the start
        // event set the flag, and now the exit event can also take the fast path).
        let event_is_read_only = trace_command_is_definitely_read_only(early_primary.as_deref());
        // For events with no command info (exit/atexit), defer to the cached flag
        // inside the lock to avoid a double-lock.
        let may_be_read_only = event_is_read_only || early_primary.is_none();
        if may_be_read_only {
            let mut ingress = match self.trace_ingress_state.lock() {
                Ok(guard) => guard,
                Err(_) => return,
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
                let payload_argv = trace_payload_argv(payload);
                if event == "start" && sid == root && !payload_argv.is_empty() {
                    ingress.root_argv.insert(root.clone(), payload_argv);
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
                return;
            }
        }

        let _ = self.mark_trace_root_activity(&root);
        let mut ingress = match self.trace_ingress_state.lock() {
            Ok(guard) => guard,
            Err(_) => {
                observability::log_error(
                    &GitAiError::Generic("trace ingress state lock poisoned".to_string()),
                    Some(serde_json::json!({
                        "component": "daemon",
                        "phase": "augment_trace_payload_with_reflog_metadata",
                        "sid": sid,
                        "event": event,
                    })),
                );
                return;
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
            return;
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
                        debug_log(&format!(
                            "daemon commit squash context capture failed sid={}: {}",
                            sid, error
                        ));
                        observability::log_error(
                            &error,
                            Some(json!({
                                "component": "daemon",
                                "phase": "augment_trace_payload_with_reflog_metadata",
                                "root_sid": root,
                                "sid": sid,
                                "argv": effective_argv,
                            })),
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
                        debug_log(&format!(
                            "daemon stash target resolution failed sid={}: {}",
                            sid, error
                        ));
                        observability::log_error(
                            &error,
                            Some(json!({
                                "component": "daemon",
                                "phase": "augment_trace_payload_with_reflog_metadata",
                                "root_sid": root,
                                "sid": sid,
                                "argv": effective_argv,
                            })),
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
                    debug_log(&format!(
                        "daemon merge --squash context capture failed sid={}: {}",
                        sid, error
                    ));
                    observability::log_error(
                        &error,
                        Some(json!({
                            "component": "daemon",
                            "phase": "augment_trace_payload_with_reflog_metadata",
                            "root_sid": root,
                            "sid": sid,
                            "argv": effective_argv,
                        })),
                    );
                }
            }
        }

        let mut ingress = match self.trace_ingress_state.lock() {
            Ok(guard) => guard,
            Err(_) => {
                observability::log_error(
                    &GitAiError::Generic("trace ingress state lock poisoned".to_string()),
                    Some(serde_json::json!({
                        "component": "daemon",
                        "phase": "augment_trace_payload_with_reflog_metadata",
                        "sid": sid,
                        "event": event,
                    })),
                );
                return;
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
                                debug_log(&format!(
                                    "daemon trace reflog delta capture error sid={}: {}",
                                    sid, error
                                ));
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
                        debug_log(&format!(
                            "daemon terminal stash target resolution failed sid={}: {}",
                            sid, error
                        ));
                        observability::log_error(
                            &error,
                            Some(json!({
                                "component": "daemon",
                                "phase": "augment_trace_payload_with_reflog_metadata",
                                "root_sid": root,
                                "sid": sid,
                                "argv": effective_argv,
                            })),
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
                        debug_log(&format!(
                            "daemon carryover snapshot capture failed sid={}: {}",
                            sid, error
                        ));
                        observability::log_error(
                            &error,
                            Some(json!({
                                "component": "daemon",
                                "phase": "augment_trace_payload_with_reflog_metadata",
                                "root_sid": root,
                                "sid": sid,
                                "argv": effective_argv,
                            })),
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
        request: CheckpointRunRequest,
        respond_to: Option<oneshot::Sender<Result<u64, GitAiError>>>,
    ) -> Result<(), GitAiError> {
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
            loop {
                let Some(first_entry) = state.entries.first_entry() else {
                    break;
                };
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
                    let result = self.coordinator.route_command(*command).await;
                    let applied = match result {
                        Ok(applied) => applied,
                        Err(error) => {
                            let _ = self.record_side_effect_error(family, order, &error);
                            debug_log(&format!(
                                "daemon command apply failed for family {} order {}: {}",
                                family, order, error
                            ));
                            continue;
                        }
                    };
                    let result = self
                        .maybe_apply_side_effects_for_applied_command(Some(family), &applied)
                        .await;
                    if let Err(error) = &result {
                        let _ = self.record_side_effect_error(family, order, error);
                        debug_log(&format!(
                            "daemon command side effect failed for family {} seq {}: {}",
                            family, applied.seq, error
                        ));
                    }
                    if let Err(error) =
                        self.append_command_completion_log(family, &applied, &result, order)
                    {
                        let _ = self.record_side_effect_error(family, order, &error);
                        debug_log(&format!(
                            "daemon command completion log write failed for family {} order {}: {}",
                            family, order, error
                        ));
                    }
                }
                FamilySequencerEntry::Checkpoint {
                    request,
                    respond_to,
                } => {
                    let captured_checkpoint_id = match request.as_ref() {
                        CheckpointRunRequest::Captured(request) => Some(request.capture_id.clone()),
                        CheckpointRunRequest::Live(_) => None,
                    };
                    let ack = self
                        .coordinator
                        .apply_checkpoint(Path::new(request.repo_working_dir()))
                        .await;
                    let should_log_completion =
                        crate::daemon::test_sync::tracks_checkpoint_request_for_test_sync(&request);
                    let mut result = match ack {
                        Ok(ack) => apply_checkpoint_side_effect(*request).map(|_| ack.seq),
                        Err(error) => Err(error),
                    };
                    if let Some(capture_id) = captured_checkpoint_id
                        && let Err(cleanup_error) =
                            crate::commands::checkpoint::delete_captured_checkpoint(&capture_id)
                    {
                        if result.is_ok() {
                            result = Err(cleanup_error);
                        } else {
                            debug_log(&format!(
                                "daemon captured checkpoint cleanup failed for family {} order {} capture {}: {}",
                                family, order, capture_id, cleanup_error
                            ));
                        }
                    }
                    if let Err(error) = &result {
                        let _ = self.record_side_effect_error(family, order, error);
                        debug_log(&format!(
                            "daemon checkpoint side effect failed for family {} order {}: {}",
                            family, order, error
                        ));
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
                            debug_log(&format!(
                                "daemon checkpoint completion log write failed for family {} order {}: {}",
                                family, order, error
                            ));
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
    ) -> Result<(), GitAiError> {
        let mut map = self
            .pending_rebase_original_head_by_worktree
            .lock()
            .map_err(|_| {
                GitAiError::Generic("pending rebase original-head map lock poisoned".to_string())
            })?;
        map.insert(Self::rewrite_worktree_key(worktree), original_head);
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

    fn resolve_stash_sha_for_event(
        cmd: &crate::daemon::domain::NormalizedCommand,
        operation: &StashOperation,
        stash_ref: Option<&str>,
    ) -> Result<Option<String>, GitAiError> {
        let resolved = match operation {
            StashOperation::Create => cmd
                .ref_changes
                .iter()
                .rfind(|change| change.reference == "refs/stash")
                .map(|change| change.new.trim().to_string())
                .filter(|oid| !oid.is_empty() && !is_zero_oid(oid)),
            StashOperation::Apply => cmd.stash_target_oid.clone().or_else(|| {
                let worktree = cmd.worktree.as_deref()?;
                resolve_stash_target_oid_for_worktree(worktree, stash_ref).or_else(|| {
                    inferred_top_stash_sha_from_rewrite_history(worktree)
                        .ok()
                        .flatten()
                })
            }),
            StashOperation::Pop | StashOperation::Drop => {
                cmd.stash_target_oid.clone().or_else(|| {
                    cmd.ref_changes
                        .iter()
                        .rfind(|change| change.reference == "refs/stash")
                        .map(|change| change.old.trim().to_string())
                        .filter(|oid| !oid.is_empty() && !is_zero_oid(oid))
                })
            }
            StashOperation::List => None,
        };
        if resolved.is_some() || !matches!(operation, StashOperation::Pop | StashOperation::Drop) {
            return Ok(resolved);
        }
        if !stash_target_spec_is_top_of_stack(stash_ref) {
            return Ok(None);
        }
        let Some(worktree) = cmd.worktree.as_deref() else {
            return Ok(None);
        };
        inferred_top_stash_sha_from_rewrite_history(worktree)
    }

    fn resolve_stash_head_for_event(
        semantic_head: Option<&String>,
        cmd: &crate::daemon::domain::NormalizedCommand,
    ) -> Option<String> {
        semantic_head
            .cloned()
            .or_else(|| cmd.pre_repo.as_ref().and_then(|repo| repo.head.clone()))
            .or_else(|| cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()))
    }

    fn resolve_stash_create_head_for_event(
        cmd: &crate::daemon::domain::NormalizedCommand,
        stash_sha: Option<&str>,
        semantic_head: Option<&String>,
    ) -> Result<Option<String>, GitAiError> {
        if let Some(stash_sha) = stash_sha
            && let Some(worktree) = cmd.worktree.as_ref()
        {
            let repo = find_repository_in_path(worktree.to_string_lossy().as_ref())?;
            let stash_commit = repo.find_commit(stash_sha.to_string())?;
            if let Ok(parent) = stash_commit.parent(0) {
                return Ok(Some(parent.id().to_string()));
            }
        }

        Ok(Self::resolve_stash_head_for_event(semantic_head, cmd))
    }

    fn resolve_stash_restore_head_for_event(
        semantic_head: Option<&String>,
        cmd: &crate::daemon::domain::NormalizedCommand,
    ) -> Option<String> {
        semantic_head
            .cloned()
            .or_else(|| cmd.pre_repo.as_ref().and_then(|repo| repo.head.clone()))
            .or_else(|| cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()))
    }

    fn stash_pathspecs_from_command(cmd: &crate::daemon::domain::NormalizedCommand) -> Vec<String> {
        let parsed = parsed_invocation_for_normalized_command(cmd);
        if parsed.command.as_deref() != Some("stash") {
            return Vec::new();
        }
        stash_hooks::extract_stash_pathspecs(&parsed)
    }

    fn merge_squash_source_ref_from_command(
        cmd: &crate::daemon::domain::NormalizedCommand,
    ) -> Option<String> {
        let parsed = parsed_invocation_for_normalized_command(cmd);
        if parsed.command.as_deref() == Some("merge")
            && parsed.command_args.iter().any(|arg| arg == "--squash")
        {
            return parsed.pos_command(0);
        }

        let raw = parse_git_cli_args(trace_invocation_args(&cmd.raw_argv));
        if raw.command.as_deref() == Some("merge")
            && raw.command_args.iter().any(|arg| arg == "--squash")
        {
            return raw.pos_command(0);
        }

        None
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

    fn resolve_merge_squash_source_head_for_event(
        cmd: &crate::daemon::domain::NormalizedCommand,
        source_ref: &str,
        source_head: &str,
    ) -> Result<String, GitAiError> {
        if !source_head.is_empty() {
            return Ok(source_head.to_string());
        }

        let worktree = cmd.worktree.as_ref().ok_or_else(|| {
            GitAiError::Generic(format!(
                "merge squash missing worktree for source resolution sid={}",
                cmd.root_sid
            ))
        })?;
        let repo = find_repository_in_path(worktree.to_string_lossy().as_ref())?;
        repo.revparse_single(source_ref)
            .and_then(|obj| obj.peel_to_commit())
            .map(|commit| commit.id())
    }

    fn synthesize_merge_squash_event_from_command(
        cmd: &crate::daemon::domain::NormalizedCommand,
    ) -> Result<Option<MergeSquashEvent>, GitAiError> {
        if cmd.exit_code != 0 {
            return Ok(None);
        }

        let parsed = parsed_invocation_for_normalized_command(cmd);
        let raw = parse_git_cli_args(trace_invocation_args(&cmd.raw_argv));
        let looks_like_squash = (parsed.command.as_deref() == Some("merge")
            && parsed.command_args.iter().any(|arg| arg == "--squash"))
            || (raw.command.as_deref() == Some("merge")
                && raw.command_args.iter().any(|arg| arg == "--squash"))
            || cmd
                .merge_squash_source_head
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty());
        if !looks_like_squash {
            return Ok(None);
        }

        let base_head = cmd
            .pre_repo
            .as_ref()
            .and_then(|repo| repo.head.clone())
            .or_else(|| cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()))
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                GitAiError::Generic(format!(
                    "merge squash fallback missing base head sid={}",
                    cmd.root_sid
                ))
            })?;
        let base_branch = cmd
            .pre_repo
            .as_ref()
            .and_then(|repo| repo.branch.clone())
            .or_else(|| cmd.post_repo.as_ref().and_then(|repo| repo.branch.clone()))
            .unwrap_or_else(|| "HEAD".to_string());
        let source_ref = Self::merge_squash_source_ref_from_command(cmd);
        let resolved_source_head = if let Some(source_head) = cmd
            .merge_squash_source_head
            .as_ref()
            .filter(|value| is_valid_oid(value) && !is_zero_oid(value))
        {
            source_head.clone()
        } else {
            let source_ref = source_ref.as_deref().ok_or_else(|| {
                GitAiError::Generic(format!(
                    "merge squash fallback missing source ref and head sid={}",
                    cmd.root_sid
                ))
            })?;
            Self::resolve_merge_squash_source_head_for_event(cmd, source_ref, "")?
        };
        Ok(Some(MergeSquashEvent::new(
            source_ref.unwrap_or_else(|| resolved_source_head.clone()),
            resolved_source_head,
            base_branch,
            base_head,
            HashMap::new(),
        )))
    }

    fn rewrite_events_from_semantic_events(
        &self,
        cmd: &crate::daemon::domain::NormalizedCommand,
        events: &[crate::daemon::domain::SemanticEvent],
    ) -> Result<Vec<RewriteLogEvent>, GitAiError> {
        let mut out = Vec::new();
        let mut implicit_merge_squash = if events.iter().any(|event| {
            matches!(
                event,
                crate::daemon::domain::SemanticEvent::MergeSquash { .. }
            )
        }) {
            None
        } else {
            Self::synthesize_merge_squash_event_from_command(cmd)?
        };
        for event in events {
            match event {
                crate::daemon::domain::SemanticEvent::CommitCreated { base, new_head } => {
                    if new_head.is_empty() {
                        return Err(GitAiError::Generic(
                            "commit created event missing new head".to_string(),
                        ));
                    }
                    if let Some(merge_squash) = implicit_merge_squash.take() {
                        out.push(RewriteLogEvent::merge_squash(merge_squash));
                    }
                    out.push(RewriteLogEvent::commit(base.clone(), new_head.clone()));
                }
                crate::daemon::domain::SemanticEvent::CommitAmended { old_head, new_head } => {
                    if old_head.is_empty()
                        || new_head.is_empty()
                        || old_head == new_head
                        || !is_valid_oid(old_head)
                        || is_zero_oid(old_head)
                        || !is_valid_oid(new_head)
                        || is_zero_oid(new_head)
                    {
                        return Err(GitAiError::Generic(
                            "commit amend event missing valid heads".to_string(),
                        ));
                    }
                    out.push(RewriteLogEvent::commit_amend(
                        old_head.clone(),
                        new_head.clone(),
                    ));
                }
                crate::daemon::domain::SemanticEvent::Reset {
                    kind,
                    old_head,
                    new_head,
                } => {
                    if old_head.is_empty() || new_head.is_empty() {
                        return Err(GitAiError::Generic(
                            "reset event missing valid heads".to_string(),
                        ));
                    }
                    let keep = matches!(kind, crate::daemon::domain::ResetKind::Keep)
                        || cmd.invoked_args.iter().any(|arg| arg == "--keep");
                    let merge = matches!(kind, crate::daemon::domain::ResetKind::Merge)
                        || cmd.invoked_args.iter().any(|arg| arg == "--merge");
                    let rewrite_kind = match kind {
                        crate::daemon::domain::ResetKind::Hard => ResetKind::Hard,
                        crate::daemon::domain::ResetKind::Soft => ResetKind::Soft,
                        _ => ResetKind::Mixed,
                    };
                    // For non-hard resets where the head actually moved, check
                    // whether the reset is really a rebase-like operation (e.g.
                    // Graphite restack on the checked-out branch).  If we can
                    // build commit mappings, emit a rebase_complete event so
                    // authorship notes get remapped -- mirroring what the wrapper
                    // does via `apply_wrapper_plumbing_rewrite_if_possible`.
                    let emitted_rebase = if !matches!(kind, crate::daemon::domain::ResetKind::Hard)
                        && old_head != new_head
                        && is_valid_oid(old_head)
                        && !is_zero_oid(old_head)
                        && is_valid_oid(new_head)
                        && !is_zero_oid(new_head)
                    {
                        if let Ok(repository) = repository_for_rewrite_context(cmd, "reset_rewrite")
                            && !is_ancestor_commit(&repository, new_head, old_head)
                        {
                            if let Some((original_commits, new_commits)) =
                                maybe_rebase_mappings_from_repository(
                                    &repository,
                                    old_head,
                                    new_head,
                                    None,
                                    "reset_rewrite",
                                )?
                            {
                                out.push(RewriteLogEvent::rebase_complete(
                                    RebaseCompleteEvent::new(
                                        old_head.clone(),
                                        new_head.clone(),
                                        false,
                                        original_commits,
                                        new_commits,
                                    ),
                                ));
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    if !emitted_rebase {
                        out.push(RewriteLogEvent::reset(ResetEvent::new(
                            rewrite_kind,
                            keep,
                            merge,
                            new_head.clone(),
                            old_head.clone(),
                        )));
                    }
                }
                crate::daemon::domain::SemanticEvent::RebaseComplete {
                    old_head,
                    new_head,
                    interactive,
                } => {
                    let worktree = cmd.worktree.as_ref().ok_or_else(|| {
                        GitAiError::Generic("rebase complete missing worktree".to_string())
                    })?;
                    let repository = repository_for_rewrite_context(cmd, "rebase_complete")?;
                    let start_target_hint = rebase_start_target_hint_from_command(cmd);
                    let Some((mapping_old_head, stable_new_head, onto_head)) =
                        Self::stable_rebase_heads_from_worktree(
                            &repository,
                            worktree,
                            &cmd.raw_argv,
                            start_target_hint.as_deref(),
                        )?
                    else {
                        debug_log(&format!(
                            "rebase complete produced no unprocessed replay segment; skipping rewrite synthesis sid={}",
                            cmd.root_sid
                        ));
                        if let Some(worktree) = cmd.worktree.as_ref() {
                            self.clear_pending_rebase_original_head_for_worktree(worktree)?;
                        }
                        continue;
                    };
                    if (!old_head.is_empty() && old_head != &mapping_old_head)
                        || (!new_head.is_empty() && new_head != &stable_new_head)
                    {
                        debug_log(&format!(
                            "rebase complete semantic heads diverged from stable reflog heads semantic_old={} semantic_new={} stable_old={} stable_new={}",
                            old_head, new_head, mapping_old_head, stable_new_head
                        ));
                    }
                    if let Some((original_commits, new_commits)) =
                        maybe_rebase_mappings_from_repository(
                            &repository,
                            &mapping_old_head,
                            &stable_new_head,
                            Some(onto_head.as_str()),
                            "rebase_complete",
                        )?
                    {
                        out.push(RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
                            mapping_old_head,
                            stable_new_head,
                            *interactive,
                            original_commits,
                            new_commits,
                        )));
                    }
                    if let Some(worktree) = cmd.worktree.as_ref() {
                        self.clear_pending_rebase_original_head_for_worktree(worktree)?;
                    }
                }
                crate::daemon::domain::SemanticEvent::RebaseAbort { head } => {
                    if !head.is_empty() {
                        out.push(RewriteLogEvent::rebase_abort(RebaseAbortEvent::new(
                            head.clone(),
                        )));
                    }
                    if let Some(worktree) = cmd.worktree.as_ref() {
                        self.clear_pending_rebase_original_head_for_worktree(worktree)?;
                    }
                }
                crate::daemon::domain::SemanticEvent::CherryPickComplete {
                    original_head,
                    new_head,
                } => {
                    if new_head.is_empty() {
                        return Err(GitAiError::Generic(
                            "cherry-pick complete event missing valid new head".to_string(),
                        ));
                    }
                    let pending_sources = cmd
                        .worktree
                        .as_ref()
                        .and_then(|worktree| {
                            self.take_pending_cherry_pick_sources_for_worktree(worktree)
                                .ok()
                        })
                        .unwrap_or_default();
                    let (resolved_original_head, source_commits, new_commits) =
                        strict_cherry_pick_mappings_from_command(
                            cmd,
                            new_head,
                            pending_sources,
                            "cherry_pick_complete",
                        )?;
                    if !original_head.is_empty() && original_head != &resolved_original_head {
                        debug_log(&format!(
                            "cherry-pick complete original head mismatch semantic={} resolved={} new={}",
                            original_head, resolved_original_head, new_head
                        ));
                    }
                    out.push(RewriteLogEvent::cherry_pick_complete(
                        CherryPickCompleteEvent::new(
                            resolved_original_head,
                            new_head.clone(),
                            source_commits,
                            new_commits,
                        ),
                    ));
                    if let Some(worktree) = cmd.worktree.as_ref() {
                        self.clear_pending_cherry_pick_sources_for_worktree(worktree)?;
                    }
                }
                crate::daemon::domain::SemanticEvent::CherryPickAbort { head } => {
                    if !head.is_empty() {
                        out.push(RewriteLogEvent::cherry_pick_abort(
                            CherryPickAbortEvent::new(head.clone()),
                        ));
                    }
                    if let Some(worktree) = cmd.worktree.as_ref() {
                        self.clear_pending_cherry_pick_sources_for_worktree(worktree)?;
                    }
                }
                crate::daemon::domain::SemanticEvent::MergeSquash {
                    base_branch,
                    base_head,
                    source_ref,
                    source_head,
                } => {
                    if base_head.is_empty() || source_ref.is_empty() {
                        return Err(GitAiError::Generic(
                            "merge squash event missing base or source".to_string(),
                        ));
                    }
                    let resolved_source_head = Self::resolve_merge_squash_source_head_for_event(
                        cmd,
                        source_ref,
                        source_head,
                    )?;
                    if !is_valid_oid(&resolved_source_head) || is_zero_oid(&resolved_source_head) {
                        return Err(GitAiError::Generic(
                            "merge squash source is not a concrete commit id".to_string(),
                        ));
                    }
                    out.push(RewriteLogEvent::merge_squash(MergeSquashEvent::new(
                        source_ref.clone(),
                        resolved_source_head,
                        base_branch.clone().unwrap_or_else(|| "HEAD".to_string()),
                        base_head.clone(),
                        HashMap::new(),
                    )));
                }
                crate::daemon::domain::SemanticEvent::StashOperation {
                    kind,
                    stash_ref,
                    head,
                } => {
                    let operation = match kind {
                        crate::daemon::domain::StashOpKind::Apply => StashOperation::Apply,
                        crate::daemon::domain::StashOpKind::Pop => StashOperation::Pop,
                        crate::daemon::domain::StashOpKind::Drop => StashOperation::Drop,
                        crate::daemon::domain::StashOpKind::List => StashOperation::List,
                        _ => StashOperation::Create,
                    };
                    let stash_sha =
                        Self::resolve_stash_sha_for_event(cmd, &operation, stash_ref.as_deref())?;
                    let head_sha = match operation {
                        StashOperation::Create => Self::resolve_stash_create_head_for_event(
                            cmd,
                            stash_sha.as_deref(),
                            head.as_ref(),
                        )?,
                        StashOperation::Apply | StashOperation::Pop => {
                            Self::resolve_stash_restore_head_for_event(head.as_ref(), cmd)
                        }
                        StashOperation::Drop | StashOperation::List => None,
                    };
                    let pathspecs = if matches!(operation, StashOperation::Create) {
                        Self::stash_pathspecs_from_command(cmd)
                    } else {
                        Vec::new()
                    };
                    if matches!(
                        operation,
                        StashOperation::Apply | StashOperation::Pop | StashOperation::Drop
                    ) && stash_sha.is_none()
                    {
                        return Err(GitAiError::Generic(format!(
                            "stash {:?} missing resolvable target oid sid={} ref={:?}",
                            operation, cmd.root_sid, stash_ref
                        )));
                    }
                    if matches!(
                        operation,
                        StashOperation::Create | StashOperation::Apply | StashOperation::Pop
                    ) && head_sha.is_none()
                    {
                        return Err(GitAiError::Generic(format!(
                            "stash {:?} missing command head sid={}",
                            operation, cmd.root_sid
                        )));
                    }
                    out.push(RewriteLogEvent::stash(StashEvent::new(
                        operation,
                        stash_ref.clone(),
                        stash_sha,
                        head_sha,
                        pathspecs,
                        true,
                        Vec::new(),
                    )));
                }
                crate::daemon::domain::SemanticEvent::PullCompleted { strategy, .. } => {
                    if matches!(
                        strategy,
                        crate::daemon::domain::PullStrategy::Rebase
                            | crate::daemon::domain::PullStrategy::RebaseMerges
                    ) {
                        let worktree = cmd.worktree.as_ref().ok_or_else(|| {
                            GitAiError::Generic("pull --rebase missing worktree".to_string())
                        })?;
                        let repository =
                            repository_for_rewrite_context(cmd, "pull_rebase_complete")?;
                        let Some((mapping_old_head, new_head, onto_head)) =
                            Self::stable_rebase_heads_from_worktree(
                                &repository,
                                worktree,
                                &cmd.raw_argv,
                                None,
                            )?
                        else {
                            debug_log(&format!(
                                "pull --rebase produced no unprocessed replay segment; skipping rewrite synthesis sid={}",
                                cmd.root_sid
                            ));
                            if let Some(worktree) = cmd.worktree.as_ref() {
                                self.clear_pending_rebase_original_head_for_worktree(worktree)?;
                            }
                            continue;
                        };
                        if let Some((original_commits, new_commits)) =
                            maybe_rebase_mappings_from_repository(
                                &repository,
                                &mapping_old_head,
                                &new_head,
                                Some(onto_head.as_str()),
                                "pull_rebase_complete",
                            )?
                        {
                            out.push(RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
                                mapping_old_head,
                                new_head,
                                false,
                                original_commits,
                                new_commits,
                            )));
                        }
                        if let Some(worktree) = cmd.worktree.as_ref() {
                            self.clear_pending_rebase_original_head_for_worktree(worktree)?;
                        }
                    }
                }
                crate::daemon::domain::SemanticEvent::RefUpdated {
                    reference,
                    old,
                    new,
                } => {
                    if reference.starts_with("refs/heads/")
                        && !old.is_empty()
                        && !new.is_empty()
                        && old != new
                        && is_valid_oid(old)
                        && !is_zero_oid(old)
                        && is_valid_oid(new)
                        && !is_zero_oid(new)
                        && let Ok(repository) =
                            repository_for_rewrite_context(cmd, "update_ref_rewrite")
                        && !is_ancestor_commit(&repository, new, old)
                        && let Some((original_commits, new_commits)) =
                            maybe_rebase_mappings_from_repository(
                                &repository,
                                old,
                                new,
                                None,
                                "update_ref_rewrite",
                            )?
                    {
                        out.push(RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
                            old.clone(),
                            new.clone(),
                            false,
                            original_commits,
                            new_commits,
                        )));
                    }
                }
                _ => {}
            }
        }

        if let Some(merge_squash) = implicit_merge_squash {
            out.push(RewriteLogEvent::merge_squash(merge_squash));
        }

        Ok(out)
    }

    async fn maybe_apply_side_effects_for_applied_command(
        &self,
        family: Option<&str>,
        applied: &crate::daemon::domain::AppliedCommand,
    ) -> Result<(), GitAiError> {
        let cmd = &applied.command;
        let events = &applied.analysis.events;
        let parsed_invocation = parsed_invocation_for_normalized_command(cmd);
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
            debug_log(&format!(
                "daemon side-effect command={} primary={} seq={} argv={:?} invoked_args={:?} ref_changes_len={} ref_changes={:?} events={:?} pre_head={:?} post_head={:?} exit_code={}",
                cmd.invoked_command.clone().unwrap_or_default(),
                cmd.primary_command.clone().unwrap_or_default(),
                applied.seq,
                cmd.raw_argv,
                cmd.invoked_args,
                cmd.ref_changes.len(),
                cmd.ref_changes,
                events,
                cmd.pre_repo.as_ref().and_then(|repo| repo.head.clone()),
                cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()),
                cmd.exit_code,
            ));
            debug_log(&format!(
                "daemon side-effect inflight_rebase_original_head={:?}",
                cmd.inflight_rebase_original_head
            ));
        }
        let carryover_snapshot = if let Some(snapshot_id) = cmd.carryover_snapshot_id.as_deref() {
            self.take_carryover_snapshot(&cmd.root_sid, snapshot_id)?
        } else {
            None
        };
        let reset_pathspecs = if cmd.primary_command.as_deref() == Some("reset") {
            let pathspecs = parsed_invocation.pathspecs();
            if pathspecs.is_empty() {
                None
            } else {
                Some(pathspecs)
            }
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
                            Some(repo.git_author_identity().name_or_unknown()),
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
                    return Err(GitAiError::Generic(format!(
                        "{} missing captured carryover snapshot for async restore",
                        cmd.primary_command.as_deref().unwrap_or("pull")
                    )));
                }
            }
        }
        if cmd.exit_code != 0 {
            if cmd.primary_command.as_deref() == Some("rebase") {
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
                        if std::env::var("GIT_AI_DEBUG_DAEMON_TRACE")
                            .ok()
                            .as_deref()
                            .is_some_and(|v| v == "1")
                        {
                            debug_log(&format!(
                                "daemon pending rebase original head set family={:?} head={}",
                                family, old_head
                            ));
                        }
                        self.set_pending_rebase_original_head_for_worktree(worktree, old_head)?;
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
            return Ok(());
        }

        if let Some(worktree) = cmd.worktree.as_ref() {
            let worktree = worktree.to_string_lossy().to_string();
            for event in events {
                match event {
                    crate::daemon::domain::SemanticEvent::CloneCompleted { .. } => {
                        if let Err(e) = apply_clone_notes_sync_side_effect(&worktree) {
                            debug_log(&format!(
                                "daemon clone notes side effect failed for {}: {}",
                                worktree, e
                            ));
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
                    _ => {}
                }
            }
        }

        let rewrite_events = match self.rewrite_events_from_semantic_events(cmd, events) {
            Ok(rewrite_events) => rewrite_events,
            Err(error) => {
                debug_log(&format!(
                    "daemon strict rewrite synthesis failed command={:?} invoked={:?} sid={} error={}",
                    cmd.primary_command, cmd.invoked_command, cmd.root_sid, error
                ));
                crate::observability::log_error(
                    &error,
                    Some(json!({
                        "component": "daemon",
                        "operation": "rewrite_events_from_semantic_events",
                        "command": cmd.primary_command,
                        "invoked_command": cmd.invoked_command,
                        "root_sid": cmd.root_sid,
                        "family": family,
                    })),
                );
                return Err(error);
            }
        };

        for rewrite_event in rewrite_events {
            if let Some(worktree) = cmd.worktree.as_ref() {
                let worktree = worktree.to_string_lossy().to_string();
                apply_rewrite_side_effect(
                    self,
                    family,
                    &worktree,
                    rewrite_event.clone(),
                    carryover_snapshot.as_ref(),
                    reset_pathspecs.as_deref(),
                )?;
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
                        debug_log(&format!(
                            "daemon async side-effect error for family {} seq {}: {}",
                            family, applied.seq, error
                        ));
                    } else if let Err(error) =
                        self.append_command_completion_log(&family, &applied, &Ok(()), applied.seq)
                    {
                        let _ = self.record_side_effect_error(&family, applied.seq, &error);
                        debug_log(&format!(
                            "daemon async completion log write failed for family {} seq {}: {}",
                            family, applied.seq, error
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    async fn ingest_checkpoint_payload(
        &self,
        request: CheckpointRunRequest,
        wait: bool,
    ) -> Result<ControlResponse, GitAiError> {
        let repo_working_dir = request.repo_working_dir().to_string();
        if repo_working_dir.trim().is_empty() {
            return Err(GitAiError::Generic(
                "checkpoint request missing repo_working_dir".to_string(),
            ));
        }
        let family = self.backend.resolve_family(Path::new(&repo_working_dir))?;
        let ingest_high_watermark = self.trace_ingest_high_watermark();
        if ingest_high_watermark > 0 {
            self.wait_for_trace_ingest_processed_through(ingest_high_watermark)
                .await?;
        }

        if wait {
            let (tx, rx) = oneshot::channel();
            self.append_checkpoint_to_family_sequencer(&family.0, request, Some(tx))
                .await?;
            let seq = rx.await.map_err(|_| {
                GitAiError::Generic("checkpoint sequencer completion receive failed".to_string())
            })??;
            return Ok(ControlResponse::ok(Some(seq), None));
        }

        self.append_checkpoint_to_family_sequencer(&family.0, request, None)
            .await?;
        Ok(ControlResponse::ok(None, None))
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
            ControlRequest::CheckpointRun { request, wait } => {
                self.ingest_checkpoint_payload(*request, wait.unwrap_or(false))
                    .await
            }
            ControlRequest::StatusFamily { repo_working_dir } => self
                .status_for_family(repo_working_dir)
                .await
                .and_then(|status| {
                    serde_json::to_value(status)
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
            ControlRequest::Shutdown => Ok(ControlResponse::ok(None, None)),
        };

        match result {
            Ok(response) => response,
            Err(error) => ControlResponse::err(error.to_string()),
        }
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
                        debug_log(&format!("daemon control connection error: {}", e));
                    }
                })
                .is_err()
            {
                debug_log("daemon control listener: failed to spawn handler thread");
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
                    debug_log(&format!("daemon control worker error: {}", error));
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
                    debug_log(&format!("daemon control worker error: {}", error));
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
                debug_log(&format!("daemon control connection error: {}", e));
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
            coordinator.request_shutdown();
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
                        debug_log(&format!("daemon trace connection error: {}", e));
                    }
                })
                .is_err()
            {
                debug_log("daemon trace listener: failed to spawn handler thread");
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
                    debug_log(&format!("daemon trace worker error: {}", error));
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
                    debug_log(&format!("daemon trace worker error: {}", error));
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
                debug_log(&format!("daemon trace connection error: {}", e));
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
        coordinator.prepare_trace_payload_for_ingest(&mut parsed);

        if coordinator.enqueue_trace_payload(parsed).is_err() {
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
                    debug_log(&format!(
                        "daemon trace connection close fallback error: {}",
                        error
                    ));
                }
            }
            Ok(_) => {}
            Err(error) => {
                debug_log(&format!(
                    "daemon trace connection close bookkeeping error: {}",
                    error
                ));
            }
        }
    }
    Ok(())
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
                debug_log("daemon update check: newer version available, requesting shutdown");
                coordinator.request_shutdown();
                return;
            }
            Ok(DaemonUpdateCheckResult::NoUpdate) => {
                debug_log("daemon update check: no update needed");
            }
            Err(err) => {
                debug_log(&format!("daemon update check failed: {}", err));
            }
        }

        let uptime_ns = now_unix_nanos().saturating_sub(started_at_ns);
        if uptime_ns >= daemon_max_uptime_ns() {
            debug_log("daemon uptime exceeded max, requesting restart");
            coordinator.request_shutdown();
            return;
        }
    }
}

/// After the daemon has fully shut down, attempt to install any pending update.
///
/// On Unix the installer atomically replaces the binary via `mv`; on Windows
/// the installer is spawned as a detached process that polls until the exe is
/// unlocked.
pub(crate) fn daemon_run_pending_self_update() {
    use crate::commands::upgrade::{
        DaemonUpdateCheckResult, check_and_install_update_if_available,
    };

    match check_and_install_update_if_available() {
        Ok(DaemonUpdateCheckResult::UpdateReady) => {
            debug_log("daemon self-update: installation completed successfully");
        }
        Ok(DaemonUpdateCheckResult::NoUpdate) => {
            debug_log("daemon self-update: no update to install");
        }
        Err(err) => {
            debug_log(&format!("daemon self-update: installation failed: {}", err));
        }
    }
}

pub async fn run_daemon(config: DaemonConfig) -> Result<(), GitAiError> {
    disable_trace2_for_daemon_process();
    config.ensure_parent_dirs()?;
    if let Err(error) = crate::commands::checkpoint::prune_stale_captured_checkpoints(
        Duration::from_secs(60 * 60 * 24),
    ) {
        debug_log(&format!(
            "daemon stale captured checkpoint pruning failed: {}",
            error
        ));
    }
    let _lock = DaemonLock::acquire(&config.lock_path)?;
    let _active_guard = DaemonProcessActiveGuard::enter();
    write_pid_metadata(&config)?;
    let _log_guard = maybe_setup_daemon_log_file(&config);
    remove_socket_if_exists(&config.trace_socket_path)?;
    remove_socket_if_exists(&config.control_socket_path)?;

    let mut coordinator_inner = ActorDaemonCoordinator::new();
    // Spawn the telemetry worker inside the daemon's tokio runtime.
    coordinator_inner.telemetry_worker =
        Some(crate::daemon::telemetry_worker::spawn_telemetry_worker());
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
                debug_log(&format!("daemon control listener exited with error: {}", e));
            }
            Err(_) => {
                debug_log("daemon control listener panicked");
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
                debug_log(&format!("daemon trace listener exited with error: {}", e));
            }
            Err(_) => {
                debug_log("daemon trace listener panicked");
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

    coordinator.wait_for_shutdown().await;

    // best effort wake listeners to allow clean process exit
    let _ = local_socket_connects_with_timeout(
        &config.control_socket_path,
        DAEMON_SOCKET_PROBE_TIMEOUT,
    );
    let _ =
        local_socket_connects_with_timeout(&config.trace_socket_path, DAEMON_SOCKET_PROBE_TIMEOUT);

    let _ = control_thread.join();
    let _ = trace_thread.join();
    let _ = update_thread.join();

    remove_socket_if_exists(&config.trace_socket_path)?;
    remove_socket_if_exists(&config.control_socket_path)?;
    remove_pid_metadata(&config)?;

    Ok(())
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
        // If the caller explicitly asked to wait for checkpoint completion, use
        // the longer budget even in product mode because the request is
        // intentionally synchronous.
        ControlRequest::CheckpointRun { wait, .. } if wait.unwrap_or(false) => {
            DAEMON_CHECKPOINT_RESPONSE_TIMEOUT
        }
        // Queued checkpoint requests can block behind trace-ingest ordering. In
        // CI/test we allow the longer budget so replay-heavy daemon tests don't
        // tear down captured state mid-request. Product mode keeps the short
        // control timeout for fire-and-forget checkpoint requests to preserve
        // responsiveness.
        ControlRequest::CheckpointRun { .. } if use_ci_or_test_budget => {
            DAEMON_CHECKPOINT_RESPONSE_TIMEOUT
        }
        ControlRequest::CheckpointRun { .. } => DAEMON_CONTROL_RESPONSE_TIMEOUT,
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

    fn queued_checkpoint_request() -> ControlRequest {
        ControlRequest::CheckpointRun {
            request: Box::new(CheckpointRunRequest::Captured(
                CapturedCheckpointRunRequest {
                    repo_working_dir: "/tmp/repo".to_string(),
                    capture_id: "capture".to_string(),
                },
            )),
            wait: Some(false),
        }
    }

    fn waited_checkpoint_request() -> ControlRequest {
        ControlRequest::CheckpointRun {
            request: Box::new(CheckpointRunRequest::Live(Box::new(
                LiveCheckpointRunRequest {
                    repo_working_dir: "/tmp/repo".to_string(),
                    kind: Some("human".to_string()),
                    author: Some("test".to_string()),
                    reset: Some(false),
                    quiet: Some(true),
                    is_pre_commit: Some(false),
                    agent_run_result: None,
                },
            ))),
            wait: Some(true),
        }
    }

    #[test]
    #[serial]
    fn checkpoint_requests_use_long_timeout_in_ci_or_test_env() {
        let _unset_ci = EnvVarGuard::unset("CI");
        let _unset_legacy_test = EnvVarGuard::unset("GITAI_TEST_DB_PATH");
        let _test_db = EnvVarGuard::set("GIT_AI_TEST_DB_PATH", "/tmp/git-ai-test.db");

        assert_eq!(
            control_request_response_timeout(&queued_checkpoint_request()),
            DAEMON_CHECKPOINT_RESPONSE_TIMEOUT
        );
        assert_eq!(
            control_request_response_timeout(&waited_checkpoint_request()),
            DAEMON_CHECKPOINT_RESPONSE_TIMEOUT
        );
    }

    #[test]
    #[serial]
    fn queued_checkpoint_requests_use_short_timeout_in_product_env() {
        let _unset_ci = EnvVarGuard::unset("CI");
        let _unset_test = EnvVarGuard::unset("GIT_AI_TEST_DB_PATH");
        let _unset_legacy_test = EnvVarGuard::unset("GITAI_TEST_DB_PATH");

        assert_eq!(
            control_request_response_timeout(&queued_checkpoint_request()),
            DAEMON_CONTROL_RESPONSE_TIMEOUT
        );
    }

    #[test]
    #[serial]
    fn waited_checkpoint_requests_use_long_timeout_in_product_env() {
        let _unset_ci = EnvVarGuard::unset("CI");
        let _unset_test = EnvVarGuard::unset("GIT_AI_TEST_DB_PATH");
        let _unset_legacy_test = EnvVarGuard::unset("GITAI_TEST_DB_PATH");

        assert_eq!(
            control_request_response_timeout(&waited_checkpoint_request()),
            DAEMON_CHECKPOINT_RESPONSE_TIMEOUT
        );
    }

    #[test]
    fn normalize_commit_carryover_snapshot_reuses_committed_blob_for_crlf_only_diff() {
        let carryover = HashMap::from([(
            "example.txt".to_string(),
            "line 1\r\nline 2\r\n".to_string(),
        )]);
        let committed =
            HashMap::from([("example.txt".to_string(), "line 1\nline 2\n".to_string())]);

        let normalized =
            normalize_commit_carryover_snapshot(Some(&carryover), Some(&committed)).unwrap();

        assert_eq!(normalized.get("example.txt"), committed.get("example.txt"));
    }

    #[test]
    fn normalize_commit_carryover_snapshot_preserves_real_post_commit_edits() {
        let carryover = HashMap::from([(
            "example.txt".to_string(),
            "line 1\r\nline 2\r\nextra line\r\n".to_string(),
        )]);
        let committed =
            HashMap::from([("example.txt".to_string(), "line 1\nline 2\n".to_string())]);

        let normalized =
            normalize_commit_carryover_snapshot(Some(&carryover), Some(&committed)).unwrap();

        assert_eq!(normalized.get("example.txt"), carryover.get("example.txt"));
    }
}
