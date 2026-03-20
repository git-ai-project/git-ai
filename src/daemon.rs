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
    HeadState, common_dir_for_worktree, git_dir_for_worktree, read_head_state_for_worktree,
    read_ref_oid_for_worktree, resolve_squash_source_head_for_worktree,
    resolve_stash_target_oid_for_worktree, worktree_root_for_path,
};
use crate::git::repository::{Repository, exec_git};
use crate::git::rewrite_log::{
    CherryPickAbortEvent, CherryPickCompleteEvent, MergeSquashEvent, RebaseAbortEvent,
    RebaseCompleteEvent, ResetEvent, ResetKind, RewriteLogEvent, StashEvent, StashOperation,
};
use crate::git::sync_authorship::{fetch_authorship_notes, fetch_remote_from_args};
use crate::observability;
use crate::utils::debug_log;
use crate::{
    authorship::rebase_authorship::{
        prepare_working_log_after_squash, reconstruct_working_log_after_reset,
    },
    authorship::working_log::CheckpointKind,
    commands::checkpoint_agent::agent_presets::AgentRunResult,
    commands::hooks::{push_hooks, stash_hooks},
};
use interprocess::local_socket::{LocalSocketListener, LocalSocketStream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc};
use tokio::time::{Duration, sleep};

pub mod analyzers;
pub mod control_api;
pub mod coordinator;
pub mod domain;
pub mod family_actor;
pub mod git_backend;
pub mod global_actor;
pub mod reducer;
pub mod trace_normalizer;

pub use control_api::{CheckpointRunRequest, ControlRequest, ControlResponse, FamilyStatus};

const PID_META_FILE: &str = "daemon.pid.json";
const TRACE_INGEST_SEQ_FIELD: &str = "git_ai_ingest_seq";
static DAEMON_PROCESS_ACTIVE: AtomicBool = AtomicBool::new(false);

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

#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
}

impl DaemonLock {
    pub fn acquire(path: &Path) -> Result<Self, GitAiError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;

        #[cfg(unix)]
        {
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                return Err(GitAiError::Generic(
                    "git-ai daemon is already running (lock held)".to_string(),
                ));
            }
        }

        Ok(Self { _file: file })
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
        }
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
    if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
        return Some(normalize(PathBuf::from(cwd)));
    }
    let argv = payload
        .get("argv")
        .and_then(Value::as_array)
        .map(|argv| {
            argv.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if argv.is_empty() {
        return None;
    }
    let mut i = 0;
    while i + 1 < argv.len() {
        if argv[i] == "-C" {
            return Some(normalize(PathBuf::from(&argv[i + 1])));
        }
        i += 1;
    }
    None
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

type MergeSquashSnapshot = (String, HashMap<String, String>);

fn capture_merge_squash_staged_file_blobs_for_command(
    worktree: &Path,
    primary_command: Option<&str>,
    argv: &[String],
    exit_code: i32,
) -> Result<Option<HashMap<String, String>>, GitAiError> {
    if exit_code != 0 || primary_command != Some("merge") {
        return Ok(None);
    }

    let parsed = parse_git_cli_args(trace_invocation_args(argv));
    if parsed.command.as_deref() != Some("merge")
        || !parsed.command_args.iter().any(|arg| arg == "--squash")
    {
        return Ok(None);
    }

    let repo = find_repository_in_path(&worktree.to_string_lossy())?;
    Ok(Some(repo.get_all_staged_file_blob_oids()?))
}

fn capture_merge_squash_source_head_for_command(
    worktree: &Path,
    primary_command: Option<&str>,
    argv: &[String],
    exit_code: i32,
) -> Result<Option<String>, GitAiError> {
    if exit_code != 0 || primary_command != Some("merge") {
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

fn capture_inflight_merge_squash_context_for_commit(
    worktree: &Path,
    primary_command: Option<&str>,
    argv: &[String],
) -> Result<Option<MergeSquashSnapshot>, GitAiError> {
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
    let repo = find_repository_in_path(&worktree.to_string_lossy())?;
    let staged_file_blobs = repo.get_all_staged_file_blob_oids()?;
    Ok(Some((source_head, staged_file_blobs)))
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
    if command == Some("stash")
        && !stash_requires_target_resolution(
            &parse_git_cli_args(trace_invocation_args(argv)).command_args,
        )
    {
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
        request.show_working_log.unwrap_or(false),
        request.reset.unwrap_or(false),
        request.quiet.unwrap_or(true),
        request.agent_run_result,
        request.is_pre_commit.unwrap_or(false),
    )?;
    Ok(())
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
    if cmd.primary_command.is_some() || !cmd.invoked_args.is_empty() {
        return parsed_invocation_for_side_effect(
            cmd.primary_command.as_deref(),
            &cmd.invoked_args,
        );
    }
    parse_git_cli_args(trace_invocation_args(&cmd.raw_argv))
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

fn build_commit_replay_file_snapshot(
    repo: &Repository,
    base_commit: &str,
    target_commit: &str,
) -> Result<(Vec<String>, HashMap<String, String>), GitAiError> {
    const EMPTY_TREE_OID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    let from_ref = if base_commit == "initial" {
        EMPTY_TREE_OID
    } else {
        base_commit
    };
    let mut files = repo.diff_changed_files(from_ref, target_commit)?;
    files.retain(|file| !file.trim().is_empty());
    files.sort();
    files.dedup();

    let mut dirty_files = HashMap::new();
    for file_path in &files {
        // Mirror wrapper pre-commit behavior: dirty file snapshots come from the current
        // worktree (which still contains unstaged edits) and only fall back to commit content.
        let worktree_content = repo.workdir().ok().and_then(|workdir| {
            let absolute = workdir.join(file_path);
            fs::read(absolute)
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        });
        let content = worktree_content
            .or_else(|| {
                repo.get_file_content(file_path, target_commit)
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
            })
            .unwrap_or_default();
        dirty_files.insert(file_path.clone(), content);
    }

    Ok((files, dirty_files))
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
                "Skipping synthetic pre-commit replay for {} to preserve tracked unstaged state",
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

fn working_log_has_tracked_state_for_base(repo: &Repository, base_commit: &str) -> bool {
    if !repo.storage.has_working_log(base_commit) {
        return false;
    }

    let working_log = repo.storage.working_log_for_base_commit(base_commit);
    let initial = working_log.read_initial_attributions();
    if !initial.files.is_empty() {
        return true;
    }

    working_log
        .read_all_checkpoints()
        .map(|checkpoints| !checkpoints.is_empty())
        .unwrap_or(false)
}

fn preceding_merge_squash_for_pending_commit(
    repo: &Repository,
    base_commit: &str,
) -> Result<Option<MergeSquashEvent>, GitAiError> {
    let events = repo.storage.read_rewrite_events()?;
    for event in events {
        match event {
            RewriteLogEvent::AuthorshipLogsSynced { .. } => continue,
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

fn seed_merge_squash_working_log_for_commit_replay(
    repo: &Repository,
    base_commit: &str,
    author: &str,
) -> Result<(), GitAiError> {
    if working_log_has_tracked_state_for_base(repo, base_commit) {
        return Ok(());
    }

    let Some(merge_squash) = preceding_merge_squash_for_pending_commit(repo, base_commit)? else {
        return Ok(());
    };

    debug_log(&format!(
        "Seeding merge --squash working log before daemon commit replay for base {}",
        base_commit
    ));
    prepare_working_log_after_squash(
        repo,
        &merge_squash.source_head,
        base_commit,
        &merge_squash.staged_file_blobs,
        author,
    )
}

fn sync_pre_commit_checkpoint_for_daemon_commit(
    repo: &Repository,
    rewrite_event: &RewriteLogEvent,
    author: &str,
) -> Result<(), GitAiError> {
    let Some((base_commit, target_commit)) =
        commit_replay_context_from_rewrite_event(rewrite_event)
    else {
        return Ok(());
    };
    if base_commit.trim().is_empty() || target_commit.trim().is_empty() || repo.workdir().is_err() {
        return Ok(());
    }
    seed_merge_squash_working_log_for_commit_replay(repo, &base_commit, author)?;
    let (changed_files, dirty_files) =
        build_commit_replay_file_snapshot(repo, &base_commit, &target_commit)?;
    let working_log = repo.storage.working_log_for_base_commit(&base_commit);
    let (changed_files, dirty_files) =
        filter_commit_replay_files(&working_log, changed_files, dirty_files);
    if changed_files.is_empty() {
        return Ok(());
    }
    let replay_agent_result = build_human_replay_agent_result(changed_files, dirty_files);

    crate::commands::checkpoint::run_with_base_commit_override(
        repo,
        author,
        CheckpointKind::Human,
        false,
        false,
        true,
        Some(replay_agent_result),
        base_commit != "initial",
        Some(base_commit.as_str()),
    )
    .map(|_| ())
}

fn apply_rewrite_side_effect(
    worktree: &str,
    rewrite_event: RewriteLogEvent,
    env_overrides: Option<&HashMap<String, String>>,
) -> Result<(), GitAiError> {
    let mut repo = find_repository_in_path(worktree)?;
    if !rewrite_event_needs_authorship_processing(&repo, &rewrite_event)? {
        let _ = repo.storage.append_rewrite_event(rewrite_event);
        return Ok(());
    }
    let author = repo.git_author_identity().name_or_unknown();
    if let RewriteLogEvent::Reset { reset } = &rewrite_event {
        apply_reset_working_log_side_effect(&repo, reset, &author)?;
    }
    if let RewriteLogEvent::Stash { stash } = &rewrite_event {
        apply_stash_rewrite_side_effect(&mut repo, stash)?;
    }
    sync_pre_commit_checkpoint_for_daemon_commit(&repo, &rewrite_event, &author)?;
    apply_env_overrides_to_working_log(&repo, &rewrite_event, env_overrides)?;
    repo.handle_rewrite_log_event(rewrite_event, author, true, true);
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
    let working_log = repo.storage.working_log_for_base_commit(&base_commit);
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

fn apply_env_overrides_to_working_log(
    repo: &crate::git::repository::Repository,
    rewrite_event: &RewriteLogEvent,
    env_overrides: Option<&HashMap<String, String>>,
) -> Result<(), GitAiError> {
    let Some(env_overrides) = env_overrides else {
        return Ok(());
    };
    let RewriteLogEvent::Commit { commit } = rewrite_event else {
        return Ok(());
    };
    let base_commit = commit
        .base_commit
        .as_deref()
        .filter(|sha| !sha.is_empty())
        .unwrap_or("initial");

    let mut changed = false;
    let working_log = repo.storage.working_log_for_base_commit(base_commit);
    let mut checkpoints = working_log.read_all_checkpoints()?;
    if checkpoints.is_empty() {
        return Ok(());
    }

    let cursor_db_override = env_overrides
        .get("GIT_AI_CURSOR_GLOBAL_DB_PATH")
        .filter(|value| !value.trim().is_empty())
        .cloned();
    let opencode_storage_override = env_overrides
        .get("GIT_AI_OPENCODE_STORAGE_PATH")
        .filter(|value| !value.trim().is_empty())
        .cloned();
    let amp_threads_override = env_overrides
        .get("GIT_AI_AMP_THREADS_PATH")
        .filter(|value| !value.trim().is_empty())
        .cloned();

    for checkpoint in &mut checkpoints {
        let Some(agent_id) = checkpoint.agent_id.as_ref() else {
            continue;
        };
        match agent_id.tool.as_str() {
            "cursor" => {
                if let Some(path) = cursor_db_override.as_ref() {
                    let metadata = checkpoint.agent_metadata.get_or_insert_with(HashMap::new);
                    metadata.insert("__test_cursor_db_path".to_string(), path.clone());
                    changed = true;
                }
            }
            "opencode" => {
                if let Some(path) = opencode_storage_override.as_ref() {
                    let metadata = checkpoint.agent_metadata.get_or_insert_with(HashMap::new);
                    metadata.insert("__test_storage_path".to_string(), path.clone());
                    changed = true;
                }
            }
            "amp" => {
                if let Some(path) = amp_threads_override.as_ref() {
                    let metadata = checkpoint.agent_metadata.get_or_insert_with(HashMap::new);
                    metadata.insert("__test_amp_threads_path".to_string(), path.clone());
                    changed = true;
                }
            }
            _ => {}
        }
    }

    if changed {
        working_log.write_all_checkpoints(&checkpoints)?;
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

fn ordered_head_commit_chain(cmd: &crate::daemon::domain::NormalizedCommand) -> Vec<String> {
    let mut out = Vec::new();
    let mut last_transition: Option<(String, String)> = None;
    for change in cmd.ref_changes.iter().filter(|change| {
        change.reference == "HEAD"
            && is_valid_oid(&change.old)
            && !is_zero_oid(&change.old)
            && is_valid_oid(&change.new)
            && !is_zero_oid(&change.new)
            && change.old != change.new
    }) {
        if last_transition
            .as_ref()
            .is_some_and(|(old, new)| old == &change.old && new == &change.new)
        {
            continue;
        }
        last_transition = Some((change.old.clone(), change.new.clone()));
        if out.last().is_none_or(|last| last != &change.old) {
            out.push(change.old.clone());
        }
        if out.last().is_none_or(|last| last != &change.new) {
            out.push(change.new.clone());
        }
    }
    out
}

fn derive_new_commits_for_rewrite(
    cmd: &crate::daemon::domain::NormalizedCommand,
    expected_count: usize,
    new_head: &str,
    context: &str,
) -> Result<Vec<String>, GitAiError> {
    if expected_count == 0 {
        return Err(GitAiError::Generic(format!(
            "{} missing source commit count",
            context
        )));
    }
    let mut commits = ordered_head_commit_chain(cmd);
    if let Some(position) = commits.iter().rposition(|oid| oid == new_head) {
        commits.truncate(position + 1);
    } else if is_valid_oid(new_head) && !is_zero_oid(new_head) {
        commits.push(new_head.to_string());
    }
    if commits.is_empty() {
        return Err(GitAiError::Generic(format!(
            "{} missing HEAD reflog commits",
            context
        )));
    }
    if commits.len() > expected_count {
        commits = commits.split_off(commits.len().saturating_sub(expected_count));
    }
    Ok(commits)
}

fn strict_rebase_mappings_from_repository(
    repository: &Repository,
    old_head: &str,
    new_head: &str,
    onto_head: Option<&str>,
    context: &str,
) -> Result<(Vec<String>, Vec<String>), GitAiError> {
    let (original_commits, new_commits) =
        crate::commands::hooks::rebase_hooks::build_rebase_commit_mappings(
            repository, old_head, new_head, onto_head,
        )?;
    if original_commits.is_empty() {
        return Err(GitAiError::Generic(format!(
            "{} missing rebase source commits",
            context
        )));
    }
    if new_commits.is_empty() {
        return Err(GitAiError::Generic(format!(
            "{} no rebased commits available for rebase mapping",
            context
        )));
    }
    Ok((original_commits, new_commits))
}

fn strict_cherry_pick_mappings_from_command(
    cmd: &crate::daemon::domain::NormalizedCommand,
    original_head: &str,
    new_head: &str,
    pending_source_commits: Vec<String>,
    context: &str,
) -> Result<(Vec<String>, Vec<String>), GitAiError> {
    if original_head.is_empty() || new_head.is_empty() || original_head == new_head {
        return Err(GitAiError::Generic(format!(
            "{} invalid cherry-pick heads old={} new={}",
            context, original_head, new_head
        )));
    }
    let mut source_commits = pending_source_commits;
    if source_commits.is_empty() {
        source_commits = cherry_pick_source_commits_from_command(cmd);
    }
    if source_commits.is_empty() {
        return Err(GitAiError::Generic(format!(
            "{} missing cherry-pick source commits",
            context
        )));
    }
    let new_commits = derive_new_commits_for_rewrite(cmd, source_commits.len(), new_head, context)?;
    Ok((source_commits, new_commits))
}

fn append_unique_oid(target: &mut Vec<String>, value: &str) {
    if is_valid_oid(value) && !is_zero_oid(value) && !target.iter().any(|seen| seen == value) {
        target.push(value.to_string());
    }
}

fn cherry_pick_source_commits_from_command(
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
        append_unique_oid(&mut out, arg);
    }
    out
}

fn rebase_is_control_mode(cmd: &crate::daemon::domain::NormalizedCommand) -> bool {
    summarize_rebase_args(&cmd.invoked_args).is_control_mode
}

fn rebase_onto_spec_from_command(cmd: &crate::daemon::domain::NormalizedCommand) -> Option<String> {
    let summary = summarize_rebase_args(&cmd.invoked_args);
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
    if is_valid_oid(semantic_old_head) && !is_zero_oid(semantic_old_head) {
        return Some(semantic_old_head.to_string());
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
) -> Result<(), GitAiError> {
    if reset.old_head_sha.is_empty() || reset.new_head_sha.is_empty() {
        return Ok(());
    }

    if reset.kind == ResetKind::Hard {
        let _ = repository
            .storage
            .delete_working_log_for_base_commit(&reset.old_head_sha);
        return Ok(());
    }

    if reset.old_head_sha == reset.new_head_sha {
        return Ok(());
    }

    let is_backward = repo_is_ancestor(repository, &reset.new_head_sha, &reset.old_head_sha);
    if is_backward {
        let _ = reconstruct_working_log_after_reset(
            repository,
            &reset.new_head_sha,
            &reset.old_head_sha,
            human_author,
            None,
        );
    } else {
        let _ = repository
            .storage
            .delete_working_log_for_base_commit(&reset.old_head_sha);
    }
    Ok(())
}

fn short_hash_json(value: &Value) -> String {
    let canonical = serde_json::to_vec(value).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical);
    format!("{:x}", hasher.finalize())
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

fn write_pid_metadata(config: &DaemonConfig) -> Result<(), GitAiError> {
    let meta = DaemonPidMeta {
        pid: std::process::id(),
        started_at_ns: now_unix_nanos(),
    };
    let path = pid_metadata_path(config);
    fs::write(path, serde_json::to_string_pretty(&meta)?)?;
    Ok(())
}

fn remove_pid_metadata(config: &DaemonConfig) -> Result<(), GitAiError> {
    let path = pid_metadata_path(config);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn read_json_line(reader: &mut BufReader<LocalSocketStream>) -> Result<Option<String>, GitAiError> {
    let mut line = String::new();
    let read = reader.read_line(&mut line)?;
    if read == 0 {
        return Ok(None);
    }
    Ok(Some(line))
}

#[derive(Debug, Clone)]
enum OrderedSideEffectEntry {
    Command(Box<crate::daemon::domain::AppliedCommand>),
    Checkpoint(Box<CheckpointRunRequest>),
    Marker,
}

#[derive(Debug, Default, Clone)]
struct FamilySideEffectState {
    next_seq: u64,
    pending: BTreeMap<u64, OrderedSideEffectEntry>,
}

#[derive(Debug, Default, Clone)]
struct TraceIngressState {
    root_worktrees: HashMap<String, PathBuf>,
    root_families: HashMap<String, String>,
    root_argv: HashMap<String, Vec<String>>,
    root_pre_repo: HashMap<String, RepoContext>,
    root_mutating: HashMap<String, bool>,
    root_target_repo_only: HashMap<String, bool>,
    root_reflog_refs: HashMap<String, Vec<String>>,
    root_head_reflog_start_offsets: HashMap<String, u64>,
    root_family_reflog_start_offsets: HashMap<String, HashMap<String, u64>>,
    root_activity_seq: HashMap<String, u64>,
    root_last_activity_ns: HashMap<String, u64>,
    root_open_connections: HashMap<String, usize>,
    root_close_fallback_enqueued: HashSet<String>,
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
    rewrite_events_by_family: Mutex<HashMap<String, Vec<Value>>>,
    pending_rebase_original_head_by_worktree: Mutex<HashMap<String, String>>,
    pending_cherry_pick_sources_by_worktree: Mutex<HashMap<String, Vec<String>>>,
    inflight_effects_by_family: Mutex<HashMap<String, usize>>,
    ordered_side_effects_by_family: Mutex<HashMap<String, FamilySideEffectState>>,
    side_effect_errors_by_family: Mutex<HashMap<String, BTreeMap<u64, String>>>,
    side_effect_progress_notify_by_family: Mutex<HashMap<String, Arc<Notify>>>,
    side_effect_exec_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    trace_ingest_tx: Mutex<Option<mpsc::Sender<Value>>>,
    next_trace_ingest_seq: AtomicUsize,
    next_trace_root_activity_seq: AtomicUsize,
    queued_trace_payloads: AtomicUsize,
    active_trace_connections: AtomicUsize,
    trace_ingress_state: Mutex<TraceIngressState>,
    shutting_down: AtomicBool,
    shutdown_notify: Notify,
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
            rewrite_events_by_family: Mutex::new(HashMap::new()),
            pending_rebase_original_head_by_worktree: Mutex::new(HashMap::new()),
            pending_cherry_pick_sources_by_worktree: Mutex::new(HashMap::new()),
            inflight_effects_by_family: Mutex::new(HashMap::new()),
            ordered_side_effects_by_family: Mutex::new(HashMap::new()),
            side_effect_errors_by_family: Mutex::new(HashMap::new()),
            side_effect_progress_notify_by_family: Mutex::new(HashMap::new()),
            side_effect_exec_locks: Mutex::new(HashMap::new()),
            trace_ingest_tx: Mutex::new(None),
            next_trace_ingest_seq: AtomicUsize::new(0),
            next_trace_root_activity_seq: AtomicUsize::new(0),
            queued_trace_payloads: AtomicUsize::new(0),
            active_trace_connections: AtomicUsize::new(0),
            trace_ingress_state: Mutex::new(TraceIngressState::default()),
            shutting_down: AtomicBool::new(false),
            shutdown_notify: Notify::new(),
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
    }

    async fn wait_for_shutdown(&self) {
        if self.is_shutting_down() {
            return;
        }
        self.shutdown_notify.notified().await;
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

    fn inflight_effect_depth(&self, family: &str) -> Result<usize, GitAiError> {
        let map = self
            .inflight_effects_by_family
            .lock()
            .map_err(|_| GitAiError::Generic("inflight effects map lock poisoned".to_string()))?;
        Ok(*map.get(family).unwrap_or(&0))
    }

    fn pending_ordered_effect_depth(&self, family: &str) -> Result<usize, GitAiError> {
        let map = self.ordered_side_effects_by_family.lock().map_err(|_| {
            GitAiError::Generic("ordered side effect map lock poisoned".to_string())
        })?;
        Ok(map
            .get(family)
            .map(|state| state.pending.len())
            .unwrap_or(0))
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

    fn trace_connection_opened(&self) {
        self.active_trace_connections.fetch_add(1, Ordering::SeqCst);
    }

    fn trace_connection_closed(&self) {
        let _ = self.active_trace_connections.fetch_update(
            Ordering::SeqCst,
            Ordering::SeqCst,
            |current| Some(current.saturating_sub(1)),
        );
    }

    fn next_trace_root_activity_seq(&self) -> u64 {
        (self
            .next_trace_root_activity_seq
            .fetch_add(1, Ordering::SeqCst) as u64)
            + 1
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

    fn mark_trace_root_activity(&self, root_sid: &str) -> Result<u64, GitAiError> {
        let activity_seq = self.next_trace_root_activity_seq();
        let mut ingress = self
            .trace_ingress_state
            .lock()
            .map_err(|_| GitAiError::Generic("trace ingress state lock poisoned".to_string()))?;
        ingress
            .root_activity_seq
            .insert(root_sid.to_string(), activity_seq);
        ingress
            .root_last_activity_ns
            .insert(root_sid.to_string(), now_unix_nanos() as u64);
        ingress.root_close_fallback_enqueued.remove(root_sid);
        Ok(activity_seq)
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

    fn record_trace_connection_close(
        &self,
        roots: &[String],
    ) -> Result<Vec<(String, u64)>, GitAiError> {
        let mut ingress = self
            .trace_ingress_state
            .lock()
            .map_err(|_| GitAiError::Generic("trace ingress state lock poisoned".to_string()))?;
        let mut stale_candidates = Vec::new();
        for root_sid in roots {
            if let Some(count) = ingress.root_open_connections.get_mut(root_sid) {
                if *count > 1 {
                    *count -= 1;
                    continue;
                }
                ingress.root_open_connections.remove(root_sid);
            }
            let activity_seq = ingress
                .root_activity_seq
                .get(root_sid)
                .copied()
                .unwrap_or(0);
            stale_candidates.push((root_sid.clone(), activity_seq));
        }
        Ok(stale_candidates)
    }

    fn enqueue_stale_connection_close_fallbacks(
        &self,
        roots: &[(String, u64)],
    ) -> Result<(), GitAiError> {
        let stale_roots = {
            let mut ingress = self.trace_ingress_state.lock().map_err(|_| {
                GitAiError::Generic("trace ingress state lock poisoned".to_string())
            })?;
            let mut stale = Vec::new();
            for (root_sid, observed_activity_seq) in roots {
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
                if ingress
                    .root_activity_seq
                    .get(root_sid)
                    .copied()
                    .unwrap_or(0)
                    != *observed_activity_seq
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
            self.augment_trace_payload_with_reflog_metadata(&mut payload);
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

    fn enqueue_idle_trace_root_fallbacks_for_family(
        &self,
        family: &str,
        min_idle_ms: u64,
    ) -> Result<(), GitAiError> {
        let min_idle_ns = min_idle_ms.saturating_mul(1_000_000);
        let now_ns = now_unix_nanos() as u64;
        let stale_roots = {
            let mut ingress = self.trace_ingress_state.lock().map_err(|_| {
                GitAiError::Generic("trace ingress state lock poisoned".to_string())
            })?;
            let mut stale = Vec::new();
            for (root_sid, tracked_family) in ingress.root_families.clone() {
                if tracked_family != family {
                    continue;
                }
                if ingress.root_close_fallback_enqueued.contains(&root_sid) {
                    continue;
                }
                if !Self::trace_root_is_tracked(&ingress, &root_sid) {
                    continue;
                }
                let last_activity_ns = ingress.root_last_activity_ns.get(&root_sid).copied();
                let idle_for_ns = match last_activity_ns {
                    Some(last) => now_ns.saturating_sub(last),
                    None => continue,
                };
                if idle_for_ns < min_idle_ns {
                    continue;
                }
                ingress
                    .root_close_fallback_enqueued
                    .insert(root_sid.clone());
                stale.push(root_sid);
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
            self.augment_trace_payload_with_reflog_metadata(&mut payload);
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    TRACE_INGEST_SEQ_FIELD.to_string(),
                    json!(self.next_trace_ingest_seq()),
                );
            }
            debug_log(&format!(
                "daemon stale trace root fallback finalized sid={}",
                root_sid
            ));
            self.enqueue_trace_payload(payload)?;
        }
        Ok(())
    }

    fn active_trace_connection_count(&self) -> u64 {
        self.active_trace_connections.load(Ordering::SeqCst) as u64
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
        ingress.root_mutating.remove(root_sid);
        ingress.root_target_repo_only.remove(root_sid);
        ingress.root_reflog_refs.remove(root_sid);
        ingress.root_head_reflog_start_offsets.remove(root_sid);
        ingress.root_family_reflog_start_offsets.remove(root_sid);
        ingress.root_activity_seq.remove(root_sid);
        ingress.root_last_activity_ns.remove(root_sid);
        ingress.root_open_connections.remove(root_sid);
        ingress.root_close_fallback_enqueued.remove(root_sid);
        Ok(())
    }

    fn next_trace_ingest_seq(&self) -> u64 {
        (self.next_trace_ingest_seq.fetch_add(1, Ordering::SeqCst) as u64) + 1
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
                    if let Some(object) = ordered_payload.as_object_mut() {
                        object.remove(TRACE_INGEST_SEQ_FIELD);
                    }

                    if let Err(error) = coordinator
                        .clone()
                        .ingest_trace_payload_fast(ordered_payload)
                        .await
                    {
                        debug_log(&format!("daemon trace ingest error: {}", error));
                    }
                    let _ = coordinator.queued_trace_payloads.fetch_update(
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                        |current| Some(current.saturating_sub(1)),
                    );
                    next_seq = next_seq.saturating_add(1);
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
            return Err(GitAiError::Generic(
                "trace ingest queue send failed".to_string(),
            ));
        }
        Ok(())
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
        if event == "start" && !payload_argv.is_empty() {
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
                ingress.root_mutating.remove(&root);
                ingress.root_target_repo_only.remove(&root);
                ingress.root_argv.remove(&root);
                ingress.root_pre_repo.remove(&root);
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
        if let Some(object) = payload.as_object_mut() {
            if let Some(repo) = pre_repo.as_ref() {
                object.insert("git_ai_pre_repo".to_string(), json!(repo));
            }
            if object.get("git_ai_merge_squash_source_head").is_none()
                && object
                    .get("git_ai_merge_squash_staged_file_blobs")
                    .is_none()
            {
                match capture_inflight_merge_squash_context_for_commit(
                    &worktree,
                    effective_primary.as_deref(),
                    &effective_argv,
                ) {
                    Ok(Some((source_head, staged_file_blobs))) => {
                        object.insert(
                            "git_ai_merge_squash_source_head".to_string(),
                            json!(source_head),
                        );
                        object.insert(
                            "git_ai_merge_squash_staged_file_blobs".to_string(),
                            json!(staged_file_blobs),
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

        if is_terminal_root_trace_event(&event, &sid, &root)
            && let Some(object) = payload.as_object_mut()
        {
            if let Some(state) = read_head_state_for_worktree(&worktree) {
                object.insert(
                    "git_ai_post_repo".to_string(),
                    json!(repo_context_from_head_state(state)),
                );
            }

            match capture_merge_squash_source_head_for_command(
                &worktree,
                effective_primary.as_deref(),
                &effective_argv,
                terminal_exit_code.unwrap_or(0),
            ) {
                Ok(Some(source_head)) => {
                    object.insert(
                        "git_ai_merge_squash_source_head".to_string(),
                        json!(source_head),
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    debug_log(&format!(
                        "daemon merge --squash source head capture failed sid={}: {}",
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
            match capture_merge_squash_staged_file_blobs_for_command(
                &worktree,
                effective_primary.as_deref(),
                &effective_argv,
                terminal_exit_code.unwrap_or(0),
            ) {
                Ok(Some(staged_file_blobs)) => {
                    object.insert(
                        "git_ai_merge_squash_staged_file_blobs".to_string(),
                        json!(staged_file_blobs),
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    debug_log(&format!(
                        "daemon merge --squash staged blob capture failed sid={}: {}",
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
                                    json!(ref_changes),
                                );
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
            ingress.root_worktrees.remove(&root);
            ingress.root_argv.remove(&root);
            ingress.root_pre_repo.remove(&root);
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

    fn side_effect_progress_notify(&self, family: &str) -> Result<Arc<Notify>, GitAiError> {
        let mut map = self
            .side_effect_progress_notify_by_family
            .lock()
            .map_err(|_| {
                GitAiError::Generic("side effect progress notify map lock poisoned".to_string())
            })?;
        Ok(map
            .entry(family.to_string())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone())
    }

    async fn enqueue_ordered_family_side_effect_entry(
        &self,
        family: &str,
        seq: u64,
        entry: OrderedSideEffectEntry,
    ) -> Result<(), GitAiError> {
        self.enqueue_ordered_family_side_effect_entry_internal(family, seq, entry, true)
            .await
    }

    async fn enqueue_ordered_family_side_effect_entry_no_drain(
        &self,
        family: &str,
        seq: u64,
        entry: OrderedSideEffectEntry,
    ) -> Result<(), GitAiError> {
        self.enqueue_ordered_family_side_effect_entry_internal(family, seq, entry, false)
            .await
    }

    async fn enqueue_ordered_family_side_effect_entry_internal(
        &self,
        family: &str,
        seq: u64,
        entry: OrderedSideEffectEntry,
        drain_now: bool,
    ) -> Result<(), GitAiError> {
        let exec_lock = self.side_effect_exec_lock(family)?;
        let _guard = exec_lock.lock().await;

        {
            let mut map = self.ordered_side_effects_by_family.lock().map_err(|_| {
                GitAiError::Generic("ordered side effect map lock poisoned".to_string())
            })?;
            let state = map
                .entry(family.to_string())
                .or_insert_with(|| FamilySideEffectState {
                    next_seq: 1,
                    pending: BTreeMap::new(),
                });
            if seq < state.next_seq {
                return Err(GitAiError::Generic(format!(
                    "ordered side effect seq regression for family {}: seq {} < next_seq {}",
                    family, seq, state.next_seq
                )));
            }
            if state.pending.insert(seq, entry).is_some() {
                return Err(GitAiError::Generic(format!(
                    "duplicate ordered side effect seq {} for family {}",
                    seq, family
                )));
            }
        }

        if drain_now {
            self.drain_ready_ordered_family_side_effect_entries_locked(family)
                .await?;
        }
        Ok(())
    }

    async fn drain_ordered_family_side_effect_entries(
        &self,
        family: &str,
    ) -> Result<(), GitAiError> {
        let exec_lock = self.side_effect_exec_lock(family)?;
        let _guard = exec_lock.lock().await;
        self.drain_ready_ordered_family_side_effect_entries_locked(family)
            .await
    }

    async fn drain_ready_ordered_family_side_effect_entries_locked(
        &self,
        family: &str,
    ) -> Result<(), GitAiError> {
        let mut ready: Vec<(u64, OrderedSideEffectEntry)> = Vec::new();
        let mut progressed = false;
        {
            let mut map = self.ordered_side_effects_by_family.lock().map_err(|_| {
                GitAiError::Generic("ordered side effect map lock poisoned".to_string())
            })?;
            let state = map
                .entry(family.to_string())
                .or_insert_with(|| FamilySideEffectState {
                    next_seq: 1,
                    pending: BTreeMap::new(),
                });
            while let Some(next_entry) = state.pending.remove(&state.next_seq) {
                let seq = state.next_seq;
                ready.push((seq, next_entry));
                state.next_seq = state.next_seq.saturating_add(1);
                progressed = true;
            }
        }

        for (seq, ready_entry) in ready {
            match ready_entry {
                OrderedSideEffectEntry::Command(applied) => {
                    let _ = self.begin_family_effect(family);
                    if let Err(error) = self
                        .maybe_apply_side_effects_for_applied_command(Some(family), &applied)
                        .await
                    {
                        let _ = self.record_side_effect_error(family, seq, &error);
                        debug_log(&format!(
                            "daemon command side effect failed for family {} seq {}: {}",
                            family, seq, error
                        ));
                    }
                    let _ = self.end_family_effect(family);
                }
                OrderedSideEffectEntry::Checkpoint(request) => {
                    let _ = self.begin_family_effect(family);
                    if let Err(error) = apply_checkpoint_side_effect(*request) {
                        let _ = self.record_side_effect_error(family, seq, &error);
                        debug_log(&format!(
                            "daemon checkpoint side effect failed for family {} seq {}: {}",
                            family, seq, error
                        ));
                    }
                    let _ = self.end_family_effect(family);
                }
                OrderedSideEffectEntry::Marker => {}
            }
        }

        if progressed {
            self.side_effect_progress_notify(family)?.notify_waiters();
        }
        Ok(())
    }

    async fn enqueue_ordered_family_side_effect_command(
        &self,
        family: &str,
        applied: crate::daemon::domain::AppliedCommand,
    ) -> Result<(), GitAiError> {
        self.enqueue_ordered_family_side_effect_entry(
            family,
            applied.seq,
            OrderedSideEffectEntry::Command(Box::new(applied)),
        )
        .await
    }

    async fn advance_ordered_family_side_effect_seq(
        &self,
        family: &str,
        seq: u64,
    ) -> Result<(), GitAiError> {
        self.enqueue_ordered_family_side_effect_entry(family, seq, OrderedSideEffectEntry::Marker)
            .await
    }

    async fn enqueue_ordered_family_checkpoint_side_effect(
        &self,
        family: &str,
        seq: u64,
        request: CheckpointRunRequest,
    ) -> Result<(), GitAiError> {
        self.enqueue_ordered_family_side_effect_entry(
            family,
            seq,
            OrderedSideEffectEntry::Checkpoint(Box::new(request)),
        )
        .await
    }

    async fn wait_for_ordered_family_side_effect_seq(
        &self,
        family: &str,
        seq: u64,
    ) -> Result<(), GitAiError> {
        let notify = self.side_effect_progress_notify(family)?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let notified = notify.notified();
            // Read progress under the family execution lock so we never observe
            // `next_seq` while ordered side effects are still in flight.
            let exec_lock = self.side_effect_exec_lock(family)?;
            let _guard = exec_lock.lock().await;
            let (applied, side_effect_error) = {
                let map = self.ordered_side_effects_by_family.lock().map_err(|_| {
                    GitAiError::Generic("ordered side effect map lock poisoned".to_string())
                })?;
                let applied = map
                    .get(family)
                    .map(|state| state.next_seq.saturating_sub(1))
                    .unwrap_or(0);
                let side_effect_error = self
                    .side_effect_errors_by_family
                    .lock()
                    .map_err(|_| {
                        GitAiError::Generic("side effect errors map lock poisoned".to_string())
                    })?
                    .get(family)
                    .and_then(|errors| errors.get(&seq).cloned());
                (applied, side_effect_error)
            };
            if let Some(error) = side_effect_error {
                return Err(GitAiError::Generic(format!(
                    "ordered side effect failed for family {} seq {}: {}",
                    family, seq, error
                )));
            }
            if applied >= seq {
                return Ok(());
            }
            drop(_guard);
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(GitAiError::Generic(format!(
                    "timed out waiting for side effects through seq {} for family {}",
                    seq, family
                )));
            }
            let wait_for = deadline.saturating_duration_since(now);
            let _ = tokio::time::timeout(wait_for, notified).await;
        }
    }

    async fn sweep_orphan_trace_roots(&self) -> Result<(), GitAiError> {
        let active_connections = self.active_trace_connection_count();
        let queued_payloads = self.queued_trace_payloads.load(Ordering::SeqCst) as u64;
        if active_connections != 0 || queued_payloads != 0 {
            return Ok(());
        }

        let removed_roots = {
            let mut normalizer = self.normalizer.lock().await;
            normalizer.sweep_orphans()
        };

        for removed in removed_roots {
            let crate::daemon::trace_normalizer::OrphanTraceRoot {
                root_sid,
                raw_argv,
                deferred_exit_only,
            } = removed;
            self.clear_trace_root_tracking(&root_sid)?;
            let error = if deferred_exit_only {
                GitAiError::Generic(format!(
                    "orphan deferred trace exit removed without active connections sid={}",
                    root_sid,
                ))
            } else {
                GitAiError::Generic(format!(
                    "orphan trace root removed without active connections sid={} argv={:?}",
                    root_sid, raw_argv
                ))
            };
            observability::log_error(
                &error,
                Some(serde_json::json!({
                    "component": "trace_normalizer",
                    "phase": if deferred_exit_only {
                        "orphan_deferred_exit_sweep"
                    } else {
                        "orphan_pending_root_sweep"
                    },
                    "root_sid": root_sid,
                    "argv": raw_argv,
                })),
            );
        }
        Ok(())
    }

    async fn family_pending_trace_root_count(&self, family: &str) -> Result<u64, GitAiError> {
        let _ = self.enqueue_idle_trace_root_fallbacks_for_family(family, 1_500);
        let has_tracked_roots = {
            let ingress = self.trace_ingress_state.lock().map_err(|_| {
                GitAiError::Generic("trace ingress state lock poisoned".to_string())
            })?;
            !ingress.root_families.is_empty()
        };
        if !has_tracked_roots {
            self.sweep_orphan_trace_roots().await?;
        }
        let ingress = self
            .trace_ingress_state
            .lock()
            .map_err(|_| GitAiError::Generic("trace ingress state lock poisoned".to_string()))?;
        Ok(ingress
            .root_families
            .values()
            .filter(|tracked_family| tracked_family.as_str() == family)
            .count() as u64)
    }

    fn append_rewrite_event_for_family(
        &self,
        family: &str,
        event: Value,
    ) -> Result<(), GitAiError> {
        let mut map = self
            .rewrite_events_by_family
            .lock()
            .map_err(|_| GitAiError::Generic("rewrite events map lock poisoned".to_string()))?;
        let entries = map.entry(family.to_string()).or_insert_with(Vec::new);
        entries.push(event);
        if entries.len() > 1024 {
            let extra = entries.len() - 1024;
            entries.drain(0..extra);
        }
        Ok(())
    }

    fn rewrite_events_for_family(&self, family: &str) -> Result<Vec<Value>, GitAiError> {
        let map = self
            .rewrite_events_by_family
            .lock()
            .map_err(|_| GitAiError::Generic("rewrite events map lock poisoned".to_string()))?;
        Ok(map.get(family).cloned().unwrap_or_default())
    }

    fn rewrite_worktree_key(worktree: &Path) -> String {
        worktree
            .canonicalize()
            .unwrap_or_else(|_| worktree.to_path_buf())
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

    fn pending_rebase_original_head_for_worktree(
        &self,
        worktree: &Path,
    ) -> Result<Option<String>, GitAiError> {
        let map = self
            .pending_rebase_original_head_by_worktree
            .lock()
            .map_err(|_| {
                GitAiError::Generic("pending rebase original-head map lock poisoned".to_string())
            })?;
        Ok(map.get(&Self::rewrite_worktree_key(worktree)).cloned())
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
    ) -> Option<String> {
        match operation {
            StashOperation::Create => cmd
                .ref_changes
                .iter()
                .rfind(|change| change.reference == "refs/stash")
                .map(|change| change.new.trim().to_string())
                .filter(|oid| !oid.is_empty() && !is_zero_oid(oid)),
            StashOperation::Apply | StashOperation::Pop | StashOperation::Drop => {
                cmd.stash_target_oid.clone()
            }
            StashOperation::List => None,
        }
    }

    fn resolve_stash_head_for_event(
        cmd: &crate::daemon::domain::NormalizedCommand,
    ) -> Option<String> {
        cmd.post_repo
            .as_ref()
            .and_then(|repo| repo.head.clone())
            .or_else(|| cmd.pre_repo.as_ref().and_then(|repo| repo.head.clone()))
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
                .is_some_and(|value| !value.trim().is_empty())
            || cmd.merge_squash_staged_file_blobs.is_some();
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
        let staged_file_blobs = cmd.merge_squash_staged_file_blobs.clone().ok_or_else(|| {
            GitAiError::Generic(format!(
                "merge squash fallback missing staged blob snapshot sid={}",
                cmd.root_sid
            ))
        })?;

        Ok(Some(MergeSquashEvent::new(
            source_ref.unwrap_or_else(|| resolved_source_head.clone()),
            resolved_source_head,
            base_branch,
            base_head,
            staged_file_blobs,
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
                    out.push(RewriteLogEvent::reset(ResetEvent::new(
                        rewrite_kind,
                        keep,
                        merge,
                        new_head.clone(),
                        old_head.clone(),
                    )));
                }
                crate::daemon::domain::SemanticEvent::RebaseComplete {
                    old_head,
                    new_head,
                    interactive,
                } => {
                    if old_head.is_empty() || new_head.is_empty() || old_head == new_head {
                        return Err(GitAiError::Generic(
                            "rebase complete event missing valid heads".to_string(),
                        ));
                    }
                    let semantic_old_valid = is_valid_oid(old_head) && !is_zero_oid(old_head);
                    let mut mapping_old_head = if semantic_old_valid {
                        old_head.clone()
                    } else {
                        strict_rebase_original_head_from_command(cmd, old_head).ok_or_else(
                            || {
                                GitAiError::Generic(
                                    "rebase complete missing valid original head".to_string(),
                                )
                            },
                        )?
                    };
                    let allow_rebase_original_override =
                        !semantic_old_valid || rebase_is_control_mode(cmd);
                    if allow_rebase_original_override {
                        if let Some(worktree) = cmd.worktree.as_ref()
                            && let Some(pending_head) =
                                self.pending_rebase_original_head_for_worktree(worktree)?
                            && pending_head != *old_head
                            && pending_head != *new_head
                        {
                            mapping_old_head = pending_head;
                        } else if let Some(inflight_head) =
                            cmd.inflight_rebase_original_head.as_ref()
                            && is_valid_oid(inflight_head)
                            && !is_zero_oid(inflight_head)
                            && inflight_head != old_head
                            && inflight_head != new_head
                        {
                            mapping_old_head = inflight_head.clone();
                        }
                    }
                    let repository = repository_for_rewrite_context(cmd, "rebase_complete")?;
                    let onto_head = rebase_onto_spec_from_command(cmd);
                    let (original_commits, new_commits) = strict_rebase_mappings_from_repository(
                        &repository,
                        &mapping_old_head,
                        new_head,
                        onto_head.as_deref(),
                        "rebase_complete",
                    )?;
                    out.push(RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
                        mapping_old_head,
                        new_head.clone(),
                        *interactive,
                        original_commits,
                        new_commits,
                    )));
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
                    if original_head.is_empty() || new_head.is_empty() || original_head == new_head
                    {
                        return Err(GitAiError::Generic(
                            "cherry-pick complete event missing valid heads".to_string(),
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
                    let (source_commits, new_commits) = strict_cherry_pick_mappings_from_command(
                        cmd,
                        original_head,
                        new_head,
                        pending_sources,
                        "cherry_pick_complete",
                    )?;
                    out.push(RewriteLogEvent::cherry_pick_complete(
                        CherryPickCompleteEvent::new(
                            original_head.clone(),
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
                    let staged_file_blobs =
                        cmd.merge_squash_staged_file_blobs.clone().ok_or_else(|| {
                            GitAiError::Generic(format!(
                                "merge squash missing staged blob snapshot sid={}",
                                cmd.root_sid
                            ))
                        })?;
                    out.push(RewriteLogEvent::merge_squash(MergeSquashEvent::new(
                        source_ref.clone(),
                        resolved_source_head,
                        base_branch.clone().unwrap_or_else(|| "HEAD".to_string()),
                        base_head.clone(),
                        staged_file_blobs,
                    )));
                }
                crate::daemon::domain::SemanticEvent::StashOperation { kind, stash_ref } => {
                    let operation = match kind {
                        crate::daemon::domain::StashOpKind::Apply => StashOperation::Apply,
                        crate::daemon::domain::StashOpKind::Pop => StashOperation::Pop,
                        crate::daemon::domain::StashOpKind::Drop => StashOperation::Drop,
                        crate::daemon::domain::StashOpKind::List => StashOperation::List,
                        _ => StashOperation::Create,
                    };
                    let stash_sha = Self::resolve_stash_sha_for_event(cmd, &operation);
                    let head_sha = Self::resolve_stash_head_for_event(cmd);
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
                            "stash {:?} missing pre-command stash target oid sid={}",
                            operation, cmd.root_sid
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
                    let (old_head, new_head) = Self::resolve_heads_for_command(cmd);
                    if matches!(
                        strategy,
                        crate::daemon::domain::PullStrategy::Rebase
                            | crate::daemon::domain::PullStrategy::RebaseMerges
                    ) {
                        if old_head.is_empty() || new_head.is_empty() || old_head == new_head {
                            return Err(GitAiError::Generic(
                                "pull --rebase missing valid old/new head".to_string(),
                            ));
                        }
                        let semantic_old_valid = is_valid_oid(&old_head) && !is_zero_oid(&old_head);
                        let mut mapping_old_head = if semantic_old_valid {
                            old_head.clone()
                        } else {
                            strict_rebase_original_head_from_command(cmd, &old_head).ok_or_else(
                                || {
                                    GitAiError::Generic(
                                        "pull --rebase missing valid original head".to_string(),
                                    )
                                },
                            )?
                        };
                        if !semantic_old_valid {
                            if let Some(worktree) = cmd.worktree.as_ref()
                                && let Some(pending_head) =
                                    self.pending_rebase_original_head_for_worktree(worktree)?
                                && pending_head != old_head
                                && pending_head != new_head
                            {
                                mapping_old_head = pending_head;
                            } else if let Some(inflight_head) =
                                cmd.inflight_rebase_original_head.as_ref()
                                && is_valid_oid(inflight_head)
                                && !is_zero_oid(inflight_head)
                                && inflight_head != &old_head
                                && inflight_head != &new_head
                            {
                                mapping_old_head = inflight_head.clone();
                            }
                        }
                        let repository =
                            repository_for_rewrite_context(cmd, "pull_rebase_complete")?;
                        let (original_commits, new_commits) =
                            strict_rebase_mappings_from_repository(
                                &repository,
                                &mapping_old_head,
                                &new_head,
                                None,
                                "pull_rebase_complete",
                            )?;
                        out.push(RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
                            mapping_old_head,
                            new_head,
                            false,
                            original_commits,
                            new_commits,
                        )));
                        if let Some(worktree) = cmd.worktree.as_ref() {
                            self.clear_pending_rebase_original_head_for_worktree(worktree)?;
                        }
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

    async fn env_overrides_for_worktree(&self, worktree: &Path) -> Option<HashMap<String, String>> {
        let snapshot = match self.coordinator.snapshot_family(worktree).await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                debug_log(&format!(
                    "daemon failed to read family snapshot for env overrides ({}): {}",
                    worktree.display(),
                    err
                ));
                return None;
            }
        };

        if let Some(overrides) = snapshot.env_overrides.get(worktree) {
            return Some(overrides.clone());
        }

        let worktree_str = worktree.to_string_lossy();
        snapshot
            .env_overrides
            .into_iter()
            .find_map(|(key, overrides)| {
                if key.to_string_lossy() == worktree_str {
                    Some(overrides)
                } else {
                    None
                }
            })
    }

    async fn maybe_apply_side_effects_for_applied_command(
        &self,
        family: Option<&str>,
        applied: &crate::daemon::domain::AppliedCommand,
    ) -> Result<(), GitAiError> {
        let cmd = &applied.command;
        let events = &applied.analysis.events;
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
                "daemon side-effect command={} primary={} seq={} argv={:?} invoked_args={:?} ref_changes_len={} ref_changes={:?} events={:?} pre_head={:?} post_head={:?} exit_code={} wrapper_mirror={}",
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
                cmd.wrapper_mirror,
            ));
            debug_log(&format!(
                "daemon side-effect inflight_rebase_original_head={:?}",
                cmd.inflight_rebase_original_head
            ));
        }
        if cmd.wrapper_mirror || cmd.exit_code != 0 {
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
                    let source_commits = cherry_pick_source_commits_from_command(cmd);
                    self.set_pending_cherry_pick_sources_for_worktree(worktree, source_commits)?;
                }
            }
            if let Some(family) = family
                && saw_pull_event
                && !cmd.ref_changes.is_empty()
            {
                self.append_rewrite_event_for_family(
                    family,
                    json!({
                        "ref_reconcile": {
                            "command": "pull",
                            "ref_changes": cmd.ref_changes,
                        }
                    }),
                )?;
            }
            return Ok(());
        }

        if let Some(worktree) = cmd.worktree.as_ref() {
            let worktree = worktree.to_string_lossy().to_string();
            for event in events {
                match event {
                    crate::daemon::domain::SemanticEvent::CloneCompleted { .. } => {
                        let _ = apply_clone_notes_sync_side_effect(&worktree);
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

        let mut emitted_rewrite_event = false;
        for rewrite_event in rewrite_events {
            emitted_rewrite_event = true;
            if let Some(worktree) = cmd.worktree.as_ref() {
                let worktree = worktree.to_string_lossy().to_string();
                let env_overrides = self.env_overrides_for_worktree(worktree.as_ref()).await;
                apply_rewrite_side_effect(
                    &worktree,
                    rewrite_event.clone(),
                    env_overrides.as_ref(),
                )?;
            }
            if let Some(family) = family {
                self.append_rewrite_event_for_family(
                    family,
                    serde_json::to_value(rewrite_event).map_err(GitAiError::from)?,
                )?;
            }
        }

        if !emitted_rewrite_event
            && let Some(family) = family
            && saw_pull_event
        {
            let (old_head, new_head) = Self::resolve_heads_for_command(cmd);
            let has_head_delta =
                !old_head.is_empty() && !new_head.is_empty() && old_head != new_head;
            if !cmd.ref_changes.is_empty() || has_head_delta {
                self.append_rewrite_event_for_family(
                    family,
                    json!({
                        "ref_reconcile": {
                            "command": "pull",
                            "ref_changes": cmd.ref_changes,
                            "old_head": old_head,
                            "new_head": new_head,
                        }
                    }),
                )?;
            }
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
    ) -> Result<Option<crate::daemon::domain::AppliedCommand>, GitAiError> {
        let emitted = {
            let mut normalizer = self.normalizer.lock().await;
            normalizer.ingest_payload(&payload)?
        };
        let Some(command) = emitted else {
            return Ok(None);
        };
        let root_sid = command.root_sid.clone();
        match self.coordinator.route_command(command).await {
            Ok(applied) => Ok(Some(applied)),
            Err(error) => {
                let _ = self.clear_trace_root_tracking(&root_sid);
                Err(error)
            }
        }
    }

    async fn ingest_trace_payload(
        &self,
        mut payload: Value,
        wait: bool,
    ) -> Result<ControlResponse, GitAiError> {
        if !is_trace_payload(&payload) {
            return Ok(ControlResponse::ok(
                None,
                None,
                Some(json!({ "ignored": true })),
            ));
        }

        self.augment_trace_payload_with_reflog_metadata(&mut payload);
        let Some(applied) = self.apply_trace_payload_to_state(payload).await? else {
            return Ok(ControlResponse::ok(
                None,
                None,
                Some(json!({ "buffered": true })),
            ));
        };
        if let Some(family) = applied
            .command
            .family_key
            .as_ref()
            .map(|key| key.0.as_str())
        {
            self.enqueue_ordered_family_side_effect_command(family, applied.clone())
                .await?;
        }
        self.clear_trace_root_tracking(&applied.command.root_sid)?;

        if wait && let Some(worktree) = applied.command.worktree.as_ref() {
            let _ = self.coordinator.barrier_family(worktree, applied.seq).await;
        }

        Ok(ControlResponse::ok(
            Some(applied.seq),
            if wait { Some(applied.seq) } else { None },
            None,
        ))
    }

    async fn ingest_trace_payload_fast(self: Arc<Self>, payload: Value) -> Result<(), GitAiError> {
        if !is_trace_payload(&payload) {
            return Ok(());
        }

        let Some(applied) = self.apply_trace_payload_to_state(payload).await? else {
            return Ok(());
        };
        let root_sid = applied.command.root_sid.clone();
        if let Some(family) = applied.command.family_key.as_ref().map(|key| key.0.clone()) {
            self.enqueue_ordered_family_side_effect_entry_no_drain(
                &family,
                applied.seq,
                OrderedSideEffectEntry::Command(Box::new(applied)),
            )
            .await?;
            let coordinator = self.clone();
            tokio::spawn(async move {
                if let Err(error) = coordinator
                    .drain_ordered_family_side_effect_entries(&family)
                    .await
                {
                    debug_log(&format!(
                        "daemon async side-effect drain error for family {}: {}",
                        family, error
                    ));
                }
            });
        }
        self.clear_trace_root_tracking(&root_sid)?;

        Ok(())
    }

    async fn ingest_checkpoint_payload(
        &self,
        request: CheckpointRunRequest,
        wait: bool,
    ) -> Result<ControlResponse, GitAiError> {
        let repo_working_dir = request.repo_working_dir.clone();
        if repo_working_dir.trim().is_empty() {
            return Err(GitAiError::Generic(
                "checkpoint request missing repo_working_dir".to_string(),
            ));
        }
        let family = self.backend.resolve_family(Path::new(&repo_working_dir))?;
        let id = format!(
            "cp-{}",
            short_hash_json(&serde_json::to_value(&request).map_err(GitAiError::from)?)
        );
        let author = request
            .author
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        let observed = crate::daemon::domain::CheckpointObserved {
            repo_working_dir: PathBuf::from(&repo_working_dir),
            id,
            author,
            timestamp_ns: now_unix_nanos(),
            file_count: 0,
        };
        let ack = self.coordinator.apply_checkpoint(observed).await?;
        self.enqueue_ordered_family_checkpoint_side_effect(&family.0, ack.seq, request)
            .await?;

        if wait {
            self.wait_for_ordered_family_side_effect_seq(&family.0, ack.seq)
                .await?;
            self.coordinator
                .barrier_family(Path::new(&repo_working_dir), ack.seq)
                .await?;
            return Ok(ControlResponse::ok(Some(ack.seq), Some(ack.seq), None));
        }
        Ok(ControlResponse::ok(Some(ack.seq), None, None))
    }

    async fn ingest_env_override(
        &self,
        repo_working_dir: String,
        env: HashMap<String, String>,
        wait: bool,
    ) -> Result<ControlResponse, GitAiError> {
        let family = self.backend.resolve_family(Path::new(&repo_working_dir))?;
        let observed = crate::daemon::domain::EnvOverrideSet {
            repo_working_dir: PathBuf::from(&repo_working_dir),
            overrides: env,
        };
        let ack = self.coordinator.apply_env_override(observed).await?;
        self.advance_ordered_family_side_effect_seq(&family.0, ack.seq)
            .await?;
        if wait {
            self.coordinator
                .barrier_family(Path::new(&repo_working_dir), ack.seq)
                .await?;
            return Ok(ControlResponse::ok(Some(ack.seq), Some(ack.seq), None));
        }
        Ok(ControlResponse::ok(Some(ack.seq), None, None))
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
        let pending_roots = self.family_pending_trace_root_count(&family.0).await?;
        let cursor = latest_seq.saturating_sub(pending_roots);
        let backlog = pending_roots;
        let inflight_effects = self.inflight_effect_depth(&family.0)?;
        let pending_ordered_effects = self.pending_ordered_effect_depth(&family.0)?;
        let family_key = family.0;
        Ok(FamilyStatus {
            family_key: family_key.clone(),
            latest_seq,
            cursor,
            backlog,
            effect_queue_depth: inflight_effects.saturating_add(pending_ordered_effects),
            active_trace_connections: 0,
            pending_roots: pending_roots as usize,
            last_error: status
                .last_error
                .or_else(|| self.latest_side_effect_error(&family_key).ok().flatten()),
        })
    }

    async fn snapshot_for_family(
        &self,
        repo_working_dir: String,
    ) -> Result<ControlResponse, GitAiError> {
        let family = self.backend.resolve_family(Path::new(&repo_working_dir))?;
        let snapshot = self
            .coordinator
            .snapshot_family(Path::new(&repo_working_dir))
            .await?;
        let latest_seq = snapshot.applied_seq;
        let rewrite_events = self.rewrite_events_for_family(&family.0)?;
        let commands = snapshot
            .recent_commands
            .iter()
            .map(|command| {
                json!({
                    "seq": command.seq,
                    "sid": command.command.root_sid,
                    "name": command.command.primary_command.clone().unwrap_or_default(),
                    "argv": command.command.raw_argv,
                    "exit_code": command.command.exit_code,
                    "worktree": command.command.worktree.as_ref().map(|p| p.to_string_lossy().to_string()),
                    "pre_head": command.command.pre_repo.as_ref().and_then(|r| r.head.clone()),
                    "post_head": command.command.post_repo.as_ref().and_then(|r| r.head.clone()),
                    "ref_changes": command.command.ref_changes,
                })
            })
            .collect::<Vec<_>>();

        Ok(ControlResponse::ok(
            None,
            None,
            Some(json!({
            "family_key": family.0,
            "latest_seq": latest_seq,
            "cursor": snapshot.applied_seq,
                "state": {
                    "commands": commands,
                    "checkpoints": snapshot.checkpoints,
                    "rewrite_events": rewrite_events,
                    "last_error": snapshot
                        .last_error
                        .or_else(|| self.latest_side_effect_error(&family.0).ok().flatten()),
                }
            })),
        ))
    }

    async fn wait_through_seq(
        &self,
        repo_working_dir: String,
        seq: u64,
    ) -> Result<ControlResponse, GitAiError> {
        let repo_path = Path::new(&repo_working_dir);
        self.coordinator.barrier_family(repo_path, seq).await?;
        let status = self.coordinator.status_family(repo_path).await?;
        Ok(ControlResponse::ok(
            Some(seq),
            Some(status.applied_seq),
            None,
        ))
    }

    async fn wait_for_family_idle(
        &self,
        repo_working_dir: String,
        timeout_ms: Option<u64>,
    ) -> Result<ControlResponse, GitAiError> {
        let timeout_polls = timeout_ms.unwrap_or(8_000) / 10;
        let mut polls_remaining = timeout_polls.max(1);
        let mut stable_idle_polls = 0_u8;

        loop {
            let status = self.status_for_family(repo_working_dir.clone()).await?;
            let latest_seq = status.latest_seq;

            if latest_seq > 0 {
                let barrier = self
                    .wait_through_seq(repo_working_dir.clone(), latest_seq)
                    .await?;
                if !barrier.ok {
                    return Ok(barrier);
                }
            }

            let settled = self.status_for_family(repo_working_dir.clone()).await?;
            let settled_latest = settled.latest_seq;
            let settled_backlog = settled.backlog;
            let settled_pending_roots = settled.pending_roots as u64;
            let settled_effect_queue_depth = settled.effect_queue_depth;

            if settled_backlog == 0
                && settled_pending_roots == 0
                && settled_effect_queue_depth == 0
                && settled_latest == latest_seq
            {
                stable_idle_polls = stable_idle_polls.saturating_add(1);
                if stable_idle_polls >= 2 {
                    return Ok(ControlResponse::ok(
                        Some(settled_latest),
                        Some(settled_latest),
                        Some(json!({
                            "latest_seq": settled_latest,
                            "backlog": settled_backlog,
                            "pending_roots": settled_pending_roots,
                            "effect_queue_depth": settled_effect_queue_depth,
                        })),
                    ));
                }
            } else {
                stable_idle_polls = 0;
            }

            if polls_remaining == 0 {
                return Err(GitAiError::Generic(format!(
                    "family {} did not settle (latest seq={}, backlog={}, pending_roots={}, effect_queue_depth={})",
                    repo_working_dir,
                    settled_latest,
                    settled_backlog,
                    settled_pending_roots,
                    settled_effect_queue_depth
                )));
            }
            polls_remaining -= 1;
            sleep(Duration::from_millis(10)).await;
        }
    }

    async fn handle_control_request(&self, request: ControlRequest) -> ControlResponse {
        let result = match request {
            ControlRequest::TraceIngest { payload, wait } => {
                self.ingest_trace_payload(payload, wait.unwrap_or(false))
                    .await
            }
            ControlRequest::CheckpointRun { request, wait } => {
                self.ingest_checkpoint_payload(*request, wait.unwrap_or(false))
                    .await
            }
            ControlRequest::EnvOverride {
                repo_working_dir,
                env,
                wait,
            } => {
                self.ingest_env_override(repo_working_dir, env, wait.unwrap_or(false))
                    .await
            }
            ControlRequest::StatusFamily { repo_working_dir } => self
                .status_for_family(repo_working_dir)
                .await
                .and_then(|status| {
                    serde_json::to_value(status)
                        .map(|v| ControlResponse::ok(None, None, Some(v)))
                        .map_err(GitAiError::from)
                }),
            ControlRequest::SnapshotFamily { repo_working_dir } => {
                self.snapshot_for_family(repo_working_dir).await
            }
            ControlRequest::BarrierAppliedThroughSeq {
                repo_working_dir,
                seq,
            } => self.wait_through_seq(repo_working_dir, seq).await,
            ControlRequest::WaitFamilyIdle {
                repo_working_dir,
                timeout_ms,
            } => {
                self.wait_for_family_idle(repo_working_dir, timeout_ms)
                    .await
            }
            ControlRequest::Shutdown => Ok(ControlResponse::ok(None, None, None)),
        };

        match result {
            Ok(response) => response,
            Err(error) => ControlResponse::err(error.to_string()),
        }
    }
}

fn control_listener_loop_actor(
    control_socket_path: PathBuf,
    coordinator: Arc<ActorDaemonCoordinator>,
    runtime_handle: tokio::runtime::Handle,
) -> Result<(), GitAiError> {
    remove_socket_if_exists(&control_socket_path)?;
    let listener = LocalSocketListener::bind(control_socket_path.to_string_lossy().as_ref())
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
        std::thread::spawn(move || {
            if let Err(e) = handle_control_connection_actor(stream, coord, handle) {
                debug_log(&format!("daemon control connection error: {}", e));
            }
        });
    }
    Ok(())
}

fn handle_control_connection_actor(
    stream: LocalSocketStream,
    coordinator: Arc<ActorDaemonCoordinator>,
    runtime_handle: tokio::runtime::Handle,
) -> Result<(), GitAiError> {
    let mut reader = BufReader::new(stream);
    while let Some(line) = read_json_line(&mut reader)? {
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
    runtime_handle: tokio::runtime::Handle,
) -> Result<(), GitAiError> {
    remove_socket_if_exists(&trace_socket_path)?;
    let listener = LocalSocketListener::bind(trace_socket_path.to_string_lossy().as_ref())
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
        let handle = runtime_handle.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle_trace_connection_actor(stream, coord, handle) {
                debug_log(&format!("daemon trace connection error: {}", e));
            }
        });
    }
    Ok(())
}

fn handle_trace_connection_actor(
    stream: LocalSocketStream,
    coordinator: Arc<ActorDaemonCoordinator>,
    runtime_handle: tokio::runtime::Handle,
) -> Result<(), GitAiError> {
    coordinator.trace_connection_opened();
    struct TraceConnectionGuard {
        coordinator: Arc<ActorDaemonCoordinator>,
    }
    impl Drop for TraceConnectionGuard {
        fn drop(&mut self) {
            self.coordinator.trace_connection_closed();
        }
    }
    let _trace_connection_guard = TraceConnectionGuard {
        coordinator: coordinator.clone(),
    };

    let mut reader = BufReader::new(stream);
    let mut observed_roots = std::collections::BTreeSet::new();
    while let Some(line) = read_json_line(&mut reader)? {
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
        coordinator.augment_trace_payload_with_reflog_metadata(&mut parsed);

        if let Some(object) = parsed.as_object_mut() {
            object.insert(
                TRACE_INGEST_SEQ_FIELD.to_string(),
                json!(coordinator.next_trace_ingest_seq()),
            );
        }

        if coordinator.enqueue_trace_payload(parsed).is_err() {
            break;
        }
    }

    if !observed_roots.is_empty() {
        let roots = observed_roots.into_iter().collect::<Vec<_>>();
        match coordinator.record_trace_connection_close(&roots) {
            Ok(stale_candidates) if !stale_candidates.is_empty() => {
                let delayed_coordinator = coordinator.clone();
                runtime_handle.spawn(async move {
                    sleep(Duration::from_millis(750)).await;
                    if let Err(error) = delayed_coordinator
                        .enqueue_stale_connection_close_fallbacks(&stale_candidates)
                    {
                        debug_log(&format!(
                            "daemon trace connection close fallback error: {}",
                            error
                        ));
                    }
                });
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

pub async fn run_daemon(config: DaemonConfig) -> Result<(), GitAiError> {
    disable_trace2_for_daemon_process();
    config.ensure_parent_dirs()?;
    let _lock = DaemonLock::acquire(&config.lock_path)?;
    let _active_guard = DaemonProcessActiveGuard::enter();
    write_pid_metadata(&config)?;
    remove_socket_if_exists(&config.trace_socket_path)?;
    remove_socket_if_exists(&config.control_socket_path)?;

    let coordinator = Arc::new(ActorDaemonCoordinator::new());
    coordinator.start_trace_ingest_worker()?;
    let rt_handle = tokio::runtime::Handle::current();
    let control_socket_path = config.control_socket_path.clone();
    let trace_socket_path = config.trace_socket_path.clone();

    let control_coord = coordinator.clone();
    let control_shutdown_coord = coordinator.clone();
    let control_handle = rt_handle.clone();
    let control_thread = std::thread::spawn(move || {
        if let Err(e) =
            control_listener_loop_actor(control_socket_path, control_coord, control_handle)
        {
            debug_log(&format!("daemon control listener exited with error: {}", e));
            // Ensure the daemon exits instead of waiting forever if listener bind/loop fails.
            control_shutdown_coord.request_shutdown();
        }
    });

    let trace_coord = coordinator.clone();
    let trace_shutdown_coord = coordinator.clone();
    let trace_handle = rt_handle.clone();
    let trace_thread = std::thread::spawn(move || {
        if let Err(e) = trace_listener_loop_actor(trace_socket_path, trace_coord, trace_handle) {
            debug_log(&format!("daemon trace listener exited with error: {}", e));
            trace_shutdown_coord.request_shutdown();
        }
    });

    coordinator.wait_for_shutdown().await;

    // best effort wake listeners to allow clean process exit
    let _ = LocalSocketStream::connect(config.control_socket_path.to_string_lossy().as_ref());
    let _ = LocalSocketStream::connect(config.trace_socket_path.to_string_lossy().as_ref());

    let _ = control_thread.join();
    let _ = trace_thread.join();

    remove_socket_if_exists(&config.trace_socket_path)?;
    remove_socket_if_exists(&config.control_socket_path)?;
    remove_pid_metadata(&config)?;
    Ok(())
}

pub fn send_control_request(
    socket_path: &Path,
    request: &ControlRequest,
) -> Result<ControlResponse, GitAiError> {
    let mut stream = LocalSocketStream::connect(socket_path.to_string_lossy().as_ref())
        .map_err(|e| GitAiError::Generic(format!("failed to connect control socket: {}", e)))?;
    let body = serde_json::to_string(request)?;
    stream.write_all(body.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut response_reader = BufReader::new(stream);
    let mut line = String::new();
    response_reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Err(GitAiError::Generic(
            "empty daemon control response".to_string(),
        ));
    }
    let resp: ControlResponse = serde_json::from_str(line.trim())?;
    Ok(resp)
}
