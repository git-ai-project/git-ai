use crate::config;
use crate::daemon::git_backend::GitBackend;
use crate::error::GitAiError;
use crate::git::cli_parser::parse_git_cli_args;
use crate::git::find_repository_in_path;
use crate::git::repository::{Repository, exec_git};
use crate::git::rewrite_log::{
    CherryPickAbortEvent, CherryPickCompleteEvent, MergeSquashEvent, RebaseAbortEvent,
    RebaseCompleteEvent, ResetEvent, ResetKind, RewriteLogEvent, StashEvent, StashOperation,
};
use crate::git::sync_authorship::{fetch_authorship_notes, fetch_remote_from_args};
use crate::utils::debug_log;
use crate::{
    authorship::rebase_authorship::{reconstruct_working_log_after_reset, walk_commits_to_base},
    authorship::working_log::CheckpointKind,
    commands::checkpoint_agent::agent_presets::AgentRunResult,
    commands::hooks::{push_hooks, rebase_hooks::build_rebase_commit_mappings, stash_hooks},
};
use interprocess::local_socket::{LocalSocketListener, LocalSocketStream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex as AsyncMutex, Notify};

pub mod analyzers;
pub mod coordinator;
pub mod domain;
pub mod effects;
pub mod family_actor;
pub mod git_backend;
pub mod global_actor;
pub mod reducer;
pub mod trace_normalizer;

const PID_META_FILE: &str = "daemon.pid.json";
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DaemonMode {
    Shadow,
    Write,
}

impl Default for DaemonMode {
    fn default() -> Self {
        Self::Shadow
    }
}

impl DaemonMode {
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "shadow" | "phase1" | "1" => Some(Self::Shadow),
            "write" | "phase2" | "phase3" | "2" | "3" => Some(Self::Write),
            _ => None,
        }
    }

    pub fn apply_side_effects(self) -> bool {
        self == Self::Write
    }
}

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub internal_dir: PathBuf,
    pub lock_path: PathBuf,
    pub trace_socket_path: PathBuf,
    pub control_socket_path: PathBuf,
    pub mode: DaemonMode,
}

impl DaemonConfig {
    pub fn from_default_paths() -> Result<Self, GitAiError> {
        let internal_dir = config::internal_dir_path().ok_or_else(|| {
            GitAiError::Generic("Unable to determine ~/.git-ai/internal path".to_string())
        })?;
        let daemon_dir = internal_dir.join("daemon");
        let mode = std::env::var("GIT_AI_DAEMON_MODE")
            .ok()
            .and_then(|v| DaemonMode::from_str(v.trim()))
            .unwrap_or_default();
        Ok(Self {
            internal_dir: internal_dir.clone(),
            lock_path: daemon_dir.join("daemon.lock"),
            trace_socket_path: daemon_dir.join("trace2.sock"),
            control_socket_path: daemon_dir.join("control.sock"),
            mode,
        })
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

    pub fn with_mode(mut self, mode: DaemonMode) -> Self {
        self.mode = mode;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonPidMeta {
    pid: u32,
    started_at_ns: u128,
    mode: DaemonMode,
}

#[derive(Debug)]
pub struct DaemonLock {
    file: File,
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

        Ok(Self { file })
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ControlRequest {
    #[serde(rename = "trace.ingest")]
    TraceIngest {
        repo_working_dir: String,
        payload: Value,
        wait: Option<bool>,
    },
    #[serde(rename = "checkpoint.run")]
    CheckpointRun {
        repo_working_dir: String,
        payload: Value,
        wait: Option<bool>,
    },
    #[serde(rename = "env.override")]
    EnvOverride {
        repo_working_dir: String,
        env: HashMap<String, String>,
        wait: Option<bool>,
    },
    #[serde(rename = "status.family")]
    StatusFamily { repo_working_dir: String },
    #[serde(rename = "snapshot.family")]
    SnapshotFamily { repo_working_dir: String },
    #[serde(rename = "barrier.applied_through_seq")]
    BarrierAppliedThroughSeq { repo_working_dir: String, seq: u64 },
    #[serde(rename = "reconcile.family")]
    ReconcileFamily { repo_working_dir: String },
    #[serde(rename = "shutdown")]
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlResponse {
    fn ok(seq: Option<u64>, applied_seq: Option<u64>, data: Option<Value>) -> Self {
        Self {
            ok: true,
            seq,
            applied_seq,
            data,
            error: None,
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            seq: None,
            applied_seq: None,
            data: None,
            error: Some(msg.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FamilyStatus {
    pub family_key: String,
    pub mode: DaemonMode,
    pub latest_seq: u64,
    pub cursor: u64,
    pub backlog: u64,
    pub effect_queue_depth: usize,
    pub active_trace_connections: usize,
    pub unresolved_transcripts: Vec<String>,
    pub pending_roots: usize,
    pub deferred_root_exits: usize,
    pub last_error: Option<String>,
    pub last_reconcile_ns: Option<u128>,
}

fn is_relevant_trace_payload(payload: &Value) -> bool {
    payload
        .get("event")
        .and_then(Value::as_str)
        .is_some_and(|event| matches!(event, "start" | "def_repo" | "cmd_name" | "exec" | "exit"))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CheckpointRunPayload {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    repo_working_dir: Option<String>,
    #[serde(default)]
    show_working_log: Option<bool>,
    #[serde(default)]
    reset: Option<bool>,
    #[serde(default)]
    quiet: Option<bool>,
    #[serde(default)]
    is_pre_commit: Option<bool>,
    #[serde(default)]
    agent_run_result: Option<AgentRunResult>,
}

fn apply_checkpoint_side_effect(payload: &Value) -> Result<(), GitAiError> {
    let request: CheckpointRunPayload = serde_json::from_value(payload.clone())?;
    let repo_working_dir = request.repo_working_dir.ok_or_else(|| {
        GitAiError::Generic("checkpoint payload missing repo_working_dir".to_string())
    })?;
    let repo = find_repository_in_path(&repo_working_dir)?;
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

fn apply_push_side_effect(worktree: &str, argv: &[String]) -> Result<(), GitAiError> {
    let repo = find_repository_in_path(worktree)?;
    let parsed = parse_git_cli_args(trace_argv_invocation_tokens(argv));
    push_hooks::run_pre_push_hook_managed(&parsed, &repo);
    Ok(())
}

fn apply_fetch_notes_sync_side_effect(worktree: &str, argv: &[String]) -> Result<(), GitAiError> {
    let repo = find_repository_in_path(worktree)?;
    let parsed = parse_git_cli_args(trace_argv_invocation_tokens(argv));
    let remote = match fetch_remote_from_args(&repo, &parsed) {
        Ok(remote) => remote,
        Err(error) => {
            debug_log(&format!(
                "daemon notes sync: failed to determine remote for {}: {}",
                parsed.command.as_deref().unwrap_or("fetch/pull"),
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

fn latest_checkpoint_file_content(
    working_log: &crate::git::repo_storage::PersistedWorkingLog,
    file_path: &str,
) -> Option<String> {
    let checkpoints = working_log.read_all_checkpoints().ok()?;
    let entry = checkpoints.iter().rev().find_map(|checkpoint| {
        checkpoint
            .entries
            .iter()
            .find(|entry| entry.file == file_path)
    })?;
    working_log.get_file_version(&entry.blob_sha).ok()
}

fn filter_commit_replay_files(
    working_log: &crate::git::repo_storage::PersistedWorkingLog,
    files: Vec<String>,
    dirty_files: HashMap<String, String>,
) -> (Vec<String>, HashMap<String, String>) {
    let mut selected_files = Vec::new();
    let mut selected_dirty_files = HashMap::new();

    for file_path in files {
        let Some(target_content) = dirty_files.get(&file_path).cloned() else {
            continue;
        };

        let should_replay = match latest_checkpoint_file_content(working_log, &file_path) {
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
    use crate::commands::git_handlers::CommandHooksContext;

    let mut args = vec!["stash".to_string()];
    match stash_event.operation {
        StashOperation::Create => args.push("push".to_string()),
        StashOperation::Apply => args.push("apply".to_string()),
        StashOperation::Pop => args.push("pop".to_string()),
        StashOperation::Drop => args.push("drop".to_string()),
        StashOperation::List => args.push("list".to_string()),
    }
    if matches!(
        stash_event.operation,
        StashOperation::Apply | StashOperation::Pop
    ) && let Some(stash_ref) = stash_event.stash_ref.as_ref()
    {
        args.push(stash_ref.clone());
    }

    let parsed = parse_git_cli_args(&args);
    let context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: stash_event.stash_sha.clone(),
        push_authorship_handle: None,
        stashed_va: None,
    };
    stash_hooks::post_stash_hook(&context, &parsed, repo, success_exit_status());
    Ok(())
}

#[cfg(unix)]
fn success_exit_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn success_exit_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
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

fn trace_argv_has_executable_prefix(argv: &[String]) -> bool {
    let Some(first) = argv.first() else {
        return false;
    };
    let file_name = Path::new(first)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(first);
    file_name.eq_ignore_ascii_case("git") || file_name.eq_ignore_ascii_case("git.exe")
}

fn trace_argv_invocation_tokens(argv: &[String]) -> &[String] {
    if trace_argv_has_executable_prefix(argv) {
        &argv[1..]
    } else {
        argv
    }
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

fn build_rebase_mappings_best_effort(
    worktree: Option<&str>,
    original_head: &str,
    new_head: &str,
) -> (Vec<String>, Vec<String>) {
    if let Some(worktree) = worktree
        && let Ok(repo) = find_repository_in_path(worktree)
        && let Ok((original_commits, mut new_commits)) =
            build_rebase_commit_mappings(&repo, original_head, new_head, None)
    {
        if !original_commits.is_empty() && new_commits.len() > original_commits.len() {
            let keep = original_commits.len();
            new_commits = new_commits.split_off(new_commits.len().saturating_sub(keep));
        }

        if !original_commits.is_empty() {
            return (original_commits, new_commits);
        }
    }

    (vec![original_head.to_string()], vec![new_head.to_string()])
}

fn build_rebase_mappings_with_onto_best_effort(
    worktree: Option<&str>,
    original_head: &str,
    new_head: &str,
    onto_head: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    if let Some(worktree) = worktree
        && let Ok(repo) = find_repository_in_path(worktree)
        && let Ok((original_commits, mut new_commits)) =
            build_rebase_commit_mappings(&repo, original_head, new_head, onto_head)
    {
        if !original_commits.is_empty() && new_commits.len() > original_commits.len() {
            let keep = original_commits.len();
            new_commits = new_commits.split_off(new_commits.len().saturating_sub(keep));
        }
        if !original_commits.is_empty() {
            return (original_commits, new_commits);
        }
    }
    build_rebase_mappings_best_effort(worktree, original_head, new_head)
}

fn build_pull_rebase_mappings_best_effort(
    worktree: Option<&str>,
    original_head: &str,
    new_head: &str,
) -> (Vec<String>, Vec<String>) {
    if let Some(worktree) = worktree
        && let Ok(repo) = find_repository_in_path(worktree)
    {
        let onto_head = repo
            .revparse_single("@{upstream}")
            .and_then(|obj| obj.peel_to_commit())
            .map(|commit| commit.id())
            .ok();
        if let Ok((original_commits, mut new_commits)) =
            build_rebase_commit_mappings(&repo, original_head, new_head, onto_head.as_deref())
        {
            if !original_commits.is_empty() && new_commits.len() > original_commits.len() {
                let keep = original_commits.len();
                new_commits = new_commits.split_off(new_commits.len().saturating_sub(keep));
            }
            if !original_commits.is_empty() {
                return (original_commits, new_commits);
            }
        }
    }

    build_rebase_mappings_best_effort(worktree, original_head, new_head)
}

fn args_after_subcommand<'a>(argv: &'a [String], command: &str) -> &'a [String] {
    argv.iter()
        .position(|arg| arg == command)
        .and_then(|idx| argv.get(idx + 1..))
        .unwrap_or(&[])
}

fn cherry_pick_source_specs_from_argv(argv: &[String]) -> Vec<String> {
    let args = args_after_subcommand(argv, "cherry-pick");
    let mut specs = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            specs.extend(args.iter().skip(i + 1).cloned());
            break;
        }
        if arg.starts_with('-') {
            let takes_value = matches!(
                arg,
                "-m" | "--mainline"
                    | "--strategy"
                    | "-X"
                    | "--strategy-option"
                    | "--cleanup"
                    | "-S"
                    | "--gpg-sign"
                    | "--rerere-autoupdate"
            );
            i = i.saturating_add(if takes_value { 2 } else { 1 });
            continue;
        }
        specs.push(args[i].clone());
        i = i.saturating_add(1);
    }
    specs
}

fn resolve_commit_specs_to_oids(worktree: Option<&str>, specs: &[String]) -> Vec<String> {
    let mut resolved = Vec::new();
    if let Some(worktree) = worktree
        && let Ok(repo) = find_repository_in_path(worktree)
    {
        for spec in specs {
            if spec.contains("..") {
                let mut args = repo.global_args_for_exec();
                args.push("rev-list".to_string());
                args.push("--reverse".to_string());
                args.push(spec.clone());
                if let Ok(output) = exec_git(&args)
                    && let Ok(stdout) = String::from_utf8(output.stdout)
                {
                    for oid in stdout
                        .lines()
                        .map(str::trim)
                        .filter(|line| is_valid_oid(line))
                    {
                        if !is_zero_oid(oid) {
                            resolved.push(oid.to_string());
                        }
                    }
                    continue;
                }
            }
            if let Ok(object) = repo.revparse_single(spec)
                && let Ok(commit) = object.peel_to_commit()
            {
                resolved.push(commit.id());
                continue;
            }
            if is_valid_oid(spec) && !is_zero_oid(spec) {
                resolved.push(spec.clone());
            }
        }
        return resolved;
    }

    for spec in specs {
        if is_valid_oid(spec) && !is_zero_oid(spec) {
            resolved.push(spec.clone());
        }
    }
    resolved
}

fn cherry_pick_created_commits_best_effort(
    worktree: Option<&str>,
    original_head: &str,
    new_head: &str,
) -> Vec<String> {
    if original_head.is_empty() || new_head.is_empty() || original_head == new_head {
        return vec![];
    }
    if let Some(worktree) = worktree
        && let Ok(repo) = find_repository_in_path(worktree)
        && let Ok(mut commits) = walk_commits_to_base(&repo, new_head, original_head)
    {
        commits.reverse();
        return commits;
    }
    vec![new_head.to_string()]
}

fn align_cherry_pick_commits(
    mut source_commits: Vec<String>,
    mut new_commits: Vec<String>,
) -> (Vec<String>, Vec<String>) {
    if source_commits.is_empty() && !new_commits.is_empty() {
        source_commits.push(new_commits[0].clone());
    }
    if new_commits.is_empty() && !source_commits.is_empty() {
        new_commits.push(source_commits[0].clone());
    }
    if source_commits.len() > new_commits.len() {
        source_commits.truncate(new_commits.len());
    } else if new_commits.len() > source_commits.len() {
        let keep = source_commits.len();
        if keep > 0 {
            new_commits = new_commits.split_off(new_commits.len().saturating_sub(keep));
        }
    }
    (source_commits, new_commits)
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
fn run_git_capture(worktree: &str, args: &[&str]) -> Result<String, GitAiError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(GitAiError::Generic(format!(
            "git command failed in {}: git {}",
            worktree,
            args.join(" ")
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn resolve_squash_source_head_from_repo(repo: &Repository) -> Option<String> {
    let merge_head_path = repo.path().join("MERGE_HEAD");
    if let Ok(contents) = fs::read_to_string(&merge_head_path) {
        for line in contents.lines() {
            let candidate = line.trim();
            if is_valid_oid(candidate) && !is_zero_oid(candidate) {
                return Some(candidate.to_string());
            }
        }
    }

    let squash_msg_path = repo.path().join("SQUASH_MSG");
    if let Ok(contents) = fs::read_to_string(&squash_msg_path) {
        for line in contents.lines() {
            if let Some(rest) = line.trim_start().strip_prefix("commit ")
                && let Some(candidate) = rest.split_whitespace().next()
                && is_valid_oid(candidate)
                && !is_zero_oid(candidate)
            {
                return Some(candidate.to_string());
            }
        }
    }

    None
}

fn resolve_squash_source_head(
    worktree: Option<&str>,
    parsed_invocation: &crate::git::cli_parser::ParsedGitInvocation,
) -> Option<String> {
    let worktree = worktree?;

    if let Some(source_spec) = parsed_invocation.pos_command(0)
        && let Ok(source_head) = run_git_capture(worktree, &["rev-parse", source_spec.as_str()])
        && is_valid_oid(&source_head)
        && !is_zero_oid(&source_head)
    {
        return Some(source_head);
    }

    find_repository_in_path(worktree)
        .ok()
        .and_then(|repo| resolve_squash_source_head_from_repo(&repo))
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
    if path.exists() {
        fs::remove_file(path)?;
    }
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
        mode: config.mode,
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
struct CherryPickProgress {
    original_head: String,
    source_commits: Vec<String>,
}

#[derive(Debug, Clone)]
struct RebaseProgress {
    original_head: String,
    onto_head: Option<String>,
    interactive: bool,
}

#[derive(Debug, Clone)]
struct RebaseArgsSummary {
    is_control_mode: bool,
    has_root: bool,
    onto_spec: Option<String>,
    positionals: Vec<String>,
}

#[derive(Debug, Default, Clone)]
struct FamilySideEffectState {
    next_seq: u64,
    pending: BTreeMap<u64, Option<crate::daemon::domain::NormalizedCommand>>,
}

struct ActorDaemonCoordinator {
    mode: DaemonMode,
    backend: Arc<crate::daemon::git_backend::SystemGitBackend>,
    coordinator:
        Arc<crate::daemon::coordinator::Coordinator<crate::daemon::git_backend::SystemGitBackend>>,
    normalizer: AsyncMutex<
        crate::daemon::trace_normalizer::TraceNormalizer<
            crate::daemon::git_backend::SystemGitBackend,
        >,
    >,
    latest_seq_by_family: Mutex<HashMap<String, u64>>,
    rewrite_events_by_family: Mutex<HashMap<String, Vec<Value>>>,
    cherry_pick_progress_by_worktree: Mutex<HashMap<String, CherryPickProgress>>,
    rebase_progress_by_worktree: Mutex<HashMap<String, RebaseProgress>>,
    inflight_effects_by_family: Mutex<HashMap<String, usize>>,
    ordered_side_effects_by_family: Mutex<HashMap<String, FamilySideEffectState>>,
    side_effect_exec_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    active_trace_connections: AtomicUsize,
    shutting_down: AtomicBool,
    shutdown_notify: Notify,
}

impl ActorDaemonCoordinator {
    fn new(mode: DaemonMode) -> Self {
        let backend = Arc::new(crate::daemon::git_backend::SystemGitBackend::new());
        Self {
            mode,
            coordinator: Arc::new(crate::daemon::coordinator::Coordinator::new(
                backend.clone(),
            )),
            normalizer: AsyncMutex::new(crate::daemon::trace_normalizer::TraceNormalizer::new(
                backend.clone(),
            )),
            backend,
            latest_seq_by_family: Mutex::new(HashMap::new()),
            rewrite_events_by_family: Mutex::new(HashMap::new()),
            cherry_pick_progress_by_worktree: Mutex::new(HashMap::new()),
            rebase_progress_by_worktree: Mutex::new(HashMap::new()),
            inflight_effects_by_family: Mutex::new(HashMap::new()),
            ordered_side_effects_by_family: Mutex::new(HashMap::new()),
            side_effect_exec_locks: Mutex::new(HashMap::new()),
            active_trace_connections: AtomicUsize::new(0),
            shutting_down: AtomicBool::new(false),
            shutdown_notify: Notify::new(),
        }
    }

    fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    fn request_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
    }

    async fn wait_for_shutdown(&self) {
        if self.is_shutting_down() {
            return;
        }
        self.shutdown_notify.notified().await;
    }

    fn update_latest_family_seq(&self, family: &str, seq: u64) -> Result<(), GitAiError> {
        let mut map = self
            .latest_seq_by_family
            .lock()
            .map_err(|_| GitAiError::Generic("latest seq map lock poisoned".to_string()))?;
        let entry = map.entry(family.to_string()).or_insert(0);
        if seq > *entry {
            *entry = seq;
        }
        Ok(())
    }

    fn bump_latest_family_seq(&self, family: &str) -> Result<u64, GitAiError> {
        let mut map = self
            .latest_seq_by_family
            .lock()
            .map_err(|_| GitAiError::Generic("latest seq map lock poisoned".to_string()))?;
        let entry = map.entry(family.to_string()).or_insert(0);
        *entry = entry.saturating_add(1);
        Ok(*entry)
    }

    fn latest_family_seq(&self, family: &str) -> Result<u64, GitAiError> {
        let map = self
            .latest_seq_by_family
            .lock()
            .map_err(|_| GitAiError::Generic("latest seq map lock poisoned".to_string()))?;
        Ok(*map.get(family).unwrap_or(&0))
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

    fn active_trace_connection_count(&self) -> u64 {
        self.active_trace_connections.load(Ordering::SeqCst) as u64
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

    async fn enqueue_ordered_family_side_effect(
        &self,
        family: &str,
        seq: u64,
        command: Option<crate::daemon::domain::NormalizedCommand>,
    ) -> Result<(), GitAiError> {
        let exec_lock = self.side_effect_exec_lock(family)?;
        let _guard = exec_lock.lock().await;

        let mut ready: Vec<Option<crate::daemon::domain::NormalizedCommand>> = Vec::new();
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
                return Ok(());
            }
            state.pending.entry(seq).or_insert(command);
            while let Some(next_command) = state.pending.remove(&state.next_seq) {
                ready.push(next_command);
                state.next_seq = state.next_seq.saturating_add(1);
            }
        }

        for command in ready.into_iter().flatten() {
            let _ = self.begin_family_effect(family);
            self.maybe_apply_side_effects_for_command(Some(family), &command)
                .await;
            let _ = self.end_family_effect(family);
        }
        Ok(())
    }

    fn resolve_family_from_payload(&self, payload: &Value) -> Option<String> {
        let worktree = payload
            .get("repo_working_dir")
            .and_then(Value::as_str)
            .or_else(|| payload.get("worktree").and_then(Value::as_str))
            .or_else(|| payload.get("repo").and_then(Value::as_str))?;
        self.backend
            .resolve_family(Path::new(worktree))
            .ok()
            .map(|family| family.0)
    }

    async fn pending_counts(&self) -> Result<(u64, u64), GitAiError> {
        let normalizer = self.normalizer.lock().await;
        Ok((
            normalizer.state().pending.len() as u64,
            normalizer.state().deferred_exits.len() as u64,
        ))
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

    fn update_cherry_pick_progress(
        &self,
        worktree: &str,
        progress: CherryPickProgress,
    ) -> Result<(), GitAiError> {
        let mut map = self.cherry_pick_progress_by_worktree.lock().map_err(|_| {
            GitAiError::Generic("cherry-pick progress map lock poisoned".to_string())
        })?;
        map.insert(worktree.to_string(), progress);
        Ok(())
    }

    fn cherry_pick_progress(
        &self,
        worktree: &str,
    ) -> Result<Option<CherryPickProgress>, GitAiError> {
        let map = self.cherry_pick_progress_by_worktree.lock().map_err(|_| {
            GitAiError::Generic("cherry-pick progress map lock poisoned".to_string())
        })?;
        Ok(map.get(worktree).cloned())
    }

    fn clear_cherry_pick_progress(&self, worktree: &str) -> Result<(), GitAiError> {
        let mut map = self.cherry_pick_progress_by_worktree.lock().map_err(|_| {
            GitAiError::Generic("cherry-pick progress map lock poisoned".to_string())
        })?;
        map.remove(worktree);
        Ok(())
    }

    fn update_rebase_progress(
        &self,
        worktree: &str,
        progress: RebaseProgress,
    ) -> Result<(), GitAiError> {
        let mut map = self
            .rebase_progress_by_worktree
            .lock()
            .map_err(|_| GitAiError::Generic("rebase progress map lock poisoned".to_string()))?;
        map.insert(worktree.to_string(), progress);
        Ok(())
    }

    fn rebase_progress(&self, worktree: &str) -> Result<Option<RebaseProgress>, GitAiError> {
        let map = self
            .rebase_progress_by_worktree
            .lock()
            .map_err(|_| GitAiError::Generic("rebase progress map lock poisoned".to_string()))?;
        Ok(map.get(worktree).cloned())
    }

    fn clear_rebase_progress(&self, worktree: &str) -> Result<(), GitAiError> {
        let mut map = self
            .rebase_progress_by_worktree
            .lock()
            .map_err(|_| GitAiError::Generic("rebase progress map lock poisoned".to_string()))?;
        map.remove(worktree);
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
                    .map(|change| change.old.clone())
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
                    .rfind(|change| change.reference == "ORIG_HEAD")
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

    fn parsed_trace_invocation(
        cmd: &crate::daemon::domain::NormalizedCommand,
    ) -> crate::git::cli_parser::ParsedGitInvocation {
        parse_git_cli_args(trace_argv_invocation_tokens(&cmd.raw_argv))
    }

    fn command_name_for_side_effects(
        cmd: &crate::daemon::domain::NormalizedCommand,
    ) -> Option<String> {
        let parsed = Self::parsed_trace_invocation(cmd);
        if let Some(invoked) = parsed.command.as_deref() {
            return Some(invoked.to_string());
        }

        cmd.primary_command
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(ToString::to_string)
    }

    fn command_args_for_side_effects(
        cmd: &crate::daemon::domain::NormalizedCommand,
        command_name: &str,
    ) -> Vec<String> {
        let parsed = Self::parsed_trace_invocation(cmd);
        let parsed_command = parsed.command.as_deref();
        if parsed_command == Some(command_name) {
            return parsed.command_args;
        }

        Self::args_after_command(&cmd.raw_argv, command_name)
    }

    fn args_without_git(argv: &[String]) -> &[String] {
        if argv.first().map(|v| v == "git").unwrap_or(false) {
            &argv[1..]
        } else {
            argv
        }
    }

    fn args_after_command(argv: &[String], command: &str) -> Vec<String> {
        let args = Self::args_without_git(argv);
        let mut seen = false;
        let mut out = Vec::new();
        for token in args {
            if !seen {
                if token == command {
                    seen = true;
                }
                continue;
            }
            out.push(token.clone());
        }
        out
    }

    fn args_have_rebase(args: &[String]) -> bool {
        args.iter().any(|arg| {
            arg == "--rebase"
                || arg == "--rebase=merges"
                || arg == "--rebase-merges"
                || arg == "--rebase=true"
        })
    }

    fn pull_uses_rebase(cmd: &crate::daemon::domain::NormalizedCommand, args: &[String]) -> bool {
        if Self::args_have_rebase(args) {
            return true;
        }

        cmd.observed_child_commands
            .iter()
            .any(|child| child.eq_ignore_ascii_case("rebase"))
    }

    fn rebase_args_summary(args: &[String]) -> RebaseArgsSummary {
        for mode in [
            "--continue",
            "--abort",
            "--skip",
            "--quit",
            "--show-current-patch",
        ] {
            if args.iter().any(|arg| arg == mode) {
                return RebaseArgsSummary {
                    is_control_mode: true,
                    has_root: false,
                    onto_spec: None,
                    positionals: Vec::new(),
                };
            }
        }

        let mut has_root = false;
        let mut onto_spec: Option<String> = None;
        let mut positionals: Vec<String> = Vec::new();
        let mut i = 0usize;

        while i < args.len() {
            let arg = args[i].as_str();
            if arg == "--" {
                break;
            }
            if arg == "--onto" {
                if let Some(next) = args.get(i + 1) {
                    onto_spec = Some(next.clone());
                    i += 2;
                    continue;
                }
                break;
            }
            if let Some(spec) = arg.strip_prefix("--onto=") {
                onto_spec = Some(spec.to_string());
                i += 1;
                continue;
            }
            if arg == "--root" {
                has_root = true;
                i += 1;
                continue;
            }
            if arg.starts_with('-') {
                let takes_value = matches!(
                    arg,
                    "-s" | "--strategy"
                        | "-X"
                        | "--strategy-option"
                        | "-x"
                        | "--exec"
                        | "--empty"
                        | "-C"
                        | "-S"
                        | "--gpg-sign"
                );
                if takes_value && !arg.contains('=') {
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }
            positionals.push(arg.to_string());
            i += 1;
        }

        RebaseArgsSummary {
            is_control_mode: false,
            has_root,
            onto_spec,
            positionals,
        }
    }

    fn rebase_target_branch_spec(summary: &RebaseArgsSummary) -> Option<&str> {
        if summary.is_control_mode {
            return None;
        }
        let branch_idx = if summary.has_root { 0 } else { 1 };
        summary.positionals.get(branch_idx).map(String::as_str)
    }

    fn rebase_target_ref_change<'a>(
        cmd: &'a crate::daemon::domain::NormalizedCommand,
        summary: &RebaseArgsSummary,
    ) -> Option<&'a crate::daemon::domain::RefChange> {
        let branch_spec = Self::rebase_target_branch_spec(summary)?;
        cmd.ref_changes.iter().find(|change| {
            change.reference == branch_spec
                || change.reference == format!("refs/heads/{}", branch_spec)
        })
    }

    fn rebase_onto_head_best_effort(
        worktree: Option<&str>,
        summary: &RebaseArgsSummary,
    ) -> Option<String> {
        if summary.is_control_mode {
            return None;
        }
        let resolve = |spec: &str| -> Option<String> {
            worktree
                .and_then(|path| run_git_capture(path, &["rev-parse", spec]).ok())
                .filter(|value| !value.trim().is_empty())
        };
        if let Some(onto_spec) = summary.onto_spec.as_deref() {
            return resolve(onto_spec);
        }
        if summary.has_root {
            return None;
        }
        if let Some(upstream_spec) = summary.positionals.first() {
            return resolve(upstream_spec);
        }
        resolve("@{upstream}")
    }

    fn observe_rebase_progress(&self, cmd: &crate::daemon::domain::NormalizedCommand) {
        let command_name = Self::command_name_for_side_effects(cmd);
        if command_name.as_deref() != Some("rebase") {
            return;
        }
        let Some(worktree) = cmd.worktree.as_ref() else {
            return;
        };
        let worktree = worktree.to_string_lossy().to_string();
        let args = Self::command_args_for_side_effects(cmd, "rebase");
        let summary = Self::rebase_args_summary(&args);
        if summary.is_control_mode {
            if args.iter().any(|arg| arg == "--quit") {
                let _ = self.clear_rebase_progress(&worktree);
            }
            return;
        }

        let (mut old_head, _) = Self::resolve_heads_for_command(cmd);
        if old_head.trim().is_empty() {
            old_head = cmd
                .post_repo
                .as_ref()
                .and_then(|repo| repo.head.clone())
                .unwrap_or_default();
        }
        if old_head.trim().is_empty() {
            old_head = run_git_capture(&worktree, &["rev-parse", "HEAD"]).unwrap_or_default();
        }
        if let Some(change) = Self::rebase_target_ref_change(cmd, &summary) {
            if !change.old.trim().is_empty() && !is_zero_oid(&change.old) {
                old_head = change.old.clone();
            }
        }

        if cmd.exit_code != 0 && !old_head.trim().is_empty() {
            let onto_head = Self::rebase_onto_head_best_effort(Some(worktree.as_str()), &summary);
            let interactive = args.iter().any(|arg| arg == "-i" || arg == "--interactive");
            let _ = self.update_rebase_progress(
                &worktree,
                RebaseProgress {
                    original_head: old_head,
                    onto_head,
                    interactive,
                },
            );
            return;
        }

        if cmd.exit_code == 0 {
            let _ = self.clear_rebase_progress(&worktree);
        }
    }

    fn infer_stash_operation(args: &[String]) -> StashOperation {
        match args.first().map(String::as_str).unwrap_or("push") {
            "push" | "save" => StashOperation::Create,
            "apply" => StashOperation::Apply,
            "pop" => StashOperation::Pop,
            "drop" => StashOperation::Drop,
            "list" => StashOperation::List,
            _ => StashOperation::Create,
        }
    }

    fn stash_ref_for_event(operation: &StashOperation, args: &[String]) -> Option<String> {
        let explicit_ref = args
            .iter()
            .find(|arg| arg.starts_with("stash@{") || is_valid_oid(arg))
            .cloned();
        if explicit_ref.is_some() {
            return explicit_ref;
        }

        if matches!(operation, StashOperation::Apply | StashOperation::Pop) {
            Some("stash@{0}".to_string())
        } else {
            None
        }
    }

    fn stash_sha_from_ref_changes(
        cmd: &crate::daemon::domain::NormalizedCommand,
    ) -> Option<String> {
        cmd.ref_changes
            .iter()
            .find(|change| {
                change.reference == "refs/stash"
                    && is_valid_oid(&change.old)
                    && !is_zero_oid(&change.old)
            })
            .map(|change| change.old.clone())
            .or_else(|| {
                cmd.ref_changes
                    .iter()
                    .find(|change| {
                        change.reference.contains("stash")
                            && is_valid_oid(&change.old)
                            && !is_zero_oid(&change.old)
                    })
                    .map(|change| change.old.clone())
            })
    }

    fn resolve_stash_sha_for_event(
        cmd: &crate::daemon::domain::NormalizedCommand,
        operation: &StashOperation,
        stash_ref: Option<&str>,
    ) -> Option<String> {
        let is_default_stash_ref = stash_ref
            .map(|reference| reference == "stash@{0}")
            .unwrap_or(true);

        if matches!(operation, StashOperation::Pop)
            && let Some(old_stash_sha) = Self::stash_sha_from_ref_changes(cmd)
        {
            return Some(old_stash_sha);
        }

        let worktree = cmd.worktree.as_ref()?.to_string_lossy().to_string();
        let stash_ref = stash_ref?;
        run_git_capture(&worktree, &["rev-parse", stash_ref])
            .ok()
            .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha))
    }

    fn observe_cherry_pick_progress(&self, cmd: &crate::daemon::domain::NormalizedCommand) {
        let command_name = Self::command_name_for_side_effects(cmd);
        if command_name.as_deref() != Some("cherry-pick") {
            return;
        }
        let Some(worktree) = cmd.worktree.as_ref() else {
            return;
        };
        let worktree = worktree.to_string_lossy().to_string();
        let args = Self::command_args_for_side_effects(cmd, "cherry-pick");
        if args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--abort" | "--quit"))
        {
            let _ = self.clear_cherry_pick_progress(&worktree);
            return;
        }
        if args.iter().any(|arg| arg == "--continue") {
            return;
        }
        if cmd.exit_code == 0 {
            return;
        }
        let (old_head, _) = Self::resolve_heads_for_command(cmd);
        if old_head.is_empty() {
            return;
        }
        let source_specs = cherry_pick_source_specs_from_argv(&cmd.raw_argv);
        let source_commits = resolve_commit_specs_to_oids(Some(&worktree), &source_specs);
        let _ = self.update_cherry_pick_progress(
            &worktree,
            CherryPickProgress {
                original_head: old_head,
                source_commits,
            },
        );
    }

    fn rewrite_event_from_command(
        &self,
        cmd: &crate::daemon::domain::NormalizedCommand,
    ) -> Option<RewriteLogEvent> {
        if cmd.exit_code != 0 {
            return None;
        }

        let name = Self::command_name_for_side_effects(cmd)?;
        let args = Self::command_args_for_side_effects(cmd, &name);
        let (old_head, new_head) = Self::resolve_heads_for_command(cmd);

        match name.as_str() {
            "commit" => {
                let is_amend = args.iter().any(|arg| arg == "--amend");
                let worktree = cmd
                    .worktree
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string());
                let resolved_new = if new_head.is_empty() {
                    worktree
                        .as_deref()
                        .and_then(|path| run_git_capture(path, &["rev-parse", "HEAD"]).ok())
                        .or_else(|| cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()))
                        .unwrap_or_default()
                } else {
                    new_head.clone()
                };
                if resolved_new.is_empty() {
                    return None;
                }

                if is_amend {
                    let resolved_old = if old_head.is_empty()
                        || old_head == resolved_new
                        || is_zero_oid(&old_head)
                    {
                        worktree
                            .as_deref()
                            .and_then(|path| run_git_capture(path, &["rev-parse", "HEAD@{1}"]).ok())
                            .unwrap_or_default()
                    } else {
                        old_head.clone()
                    };
                    if resolved_old.is_empty() || resolved_old == resolved_new {
                        return None;
                    }
                    Some(RewriteLogEvent::commit_amend(resolved_old, resolved_new))
                } else {
                    let base = if old_head.is_empty()
                        || old_head == resolved_new
                        || is_zero_oid(&old_head)
                    {
                        worktree
                            .as_deref()
                            .and_then(|path| run_git_capture(path, &["rev-parse", "HEAD^"]).ok())
                            .filter(|parent| !parent.is_empty() && parent != &resolved_new)
                    } else {
                        Some(old_head.clone())
                    };
                    Some(RewriteLogEvent::commit(base, resolved_new))
                }
            }
            "reset" => {
                let worktree = cmd
                    .worktree
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string());
                let mut resolved_old = old_head.clone();
                let mut resolved_new = new_head.clone();

                if resolved_new.is_empty() {
                    resolved_new = worktree
                        .as_deref()
                        .and_then(|path| run_git_capture(path, &["rev-parse", "HEAD"]).ok())
                        .unwrap_or_default();
                }
                if resolved_old.is_empty()
                    || resolved_old == resolved_new
                    || is_zero_oid(&resolved_old)
                {
                    resolved_old = worktree
                        .as_deref()
                        .and_then(|path| run_git_capture(path, &["rev-parse", "HEAD@{1}"]).ok())
                        .unwrap_or_default();
                }

                if resolved_old.is_empty() || resolved_new.is_empty() {
                    return None;
                }
                let kind = if args.iter().any(|arg| arg == "--hard") {
                    ResetKind::Hard
                } else if args.iter().any(|arg| arg == "--soft") {
                    ResetKind::Soft
                } else {
                    ResetKind::Mixed
                };
                Some(RewriteLogEvent::reset(ResetEvent::new(
                    kind,
                    args.iter().any(|arg| arg == "--keep"),
                    args.iter().any(|arg| arg == "--merge"),
                    resolved_new,
                    resolved_old,
                )))
            }
            "rebase" => {
                let summary = Self::rebase_args_summary(&args);
                let worktree = cmd
                    .worktree
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string());
                let tracked_progress = worktree
                    .as_deref()
                    .and_then(|path| self.rebase_progress(path).ok().flatten());

                let mut event_old_head = old_head.clone();
                let mut event_new_head = new_head.clone();
                if let Some(change) = Self::rebase_target_ref_change(cmd, &summary) {
                    if !change.old.trim().is_empty() && !is_zero_oid(&change.old) {
                        event_old_head = change.old.clone();
                    }
                    if !change.new.trim().is_empty() && !is_zero_oid(&change.new) {
                        event_new_head = change.new.clone();
                    }
                }

                if summary.is_control_mode {
                    if let Some(progress) = tracked_progress.as_ref() {
                        if !progress.original_head.trim().is_empty() {
                            event_old_head = progress.original_head.clone();
                        }
                    }
                    if event_new_head.trim().is_empty() {
                        event_new_head = cmd
                            .post_repo
                            .as_ref()
                            .and_then(|repo| repo.head.clone())
                            .unwrap_or_default();
                    }
                }

                if args.iter().any(|arg| arg == "--abort") {
                    let original = if event_old_head.is_empty() {
                        event_new_head
                    } else {
                        event_old_head
                    };
                    if let Some(worktree) = worktree.as_deref() {
                        let _ = self.clear_rebase_progress(worktree);
                    }
                    if original.is_empty() {
                        None
                    } else {
                        Some(RewriteLogEvent::rebase_abort(RebaseAbortEvent::new(
                            original,
                        )))
                    }
                } else if !event_old_head.is_empty()
                    && !event_new_head.is_empty()
                    && event_old_head != event_new_head
                {
                    let onto_head = tracked_progress
                        .as_ref()
                        .and_then(|progress| progress.onto_head.clone())
                        .or_else(|| {
                            Self::rebase_onto_head_best_effort(worktree.as_deref(), &summary)
                        });
                    let (original_commits, new_commits) =
                        build_rebase_mappings_with_onto_best_effort(
                            worktree.as_deref(),
                            &event_old_head,
                            &event_new_head,
                            onto_head.as_deref(),
                        );
                    let original_commits = if original_commits.is_empty() {
                        vec![event_old_head.clone()]
                    } else {
                        original_commits
                    };
                    if summary.is_control_mode {
                        if let Some(worktree) = worktree.as_deref() {
                            let _ = self.clear_rebase_progress(worktree);
                        }
                    }
                    Some(RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
                        event_old_head,
                        event_new_head,
                        tracked_progress
                            .as_ref()
                            .map(|progress| progress.interactive)
                            .unwrap_or_else(|| {
                                args.iter().any(|arg| arg == "-i" || arg == "--interactive")
                            }),
                        original_commits,
                        new_commits,
                    )))
                } else {
                    if summary.is_control_mode {
                        if let Some(worktree) = worktree.as_deref() {
                            let _ = self.clear_rebase_progress(worktree);
                        }
                    }
                    None
                }
            }
            "pull" => {
                if Self::pull_uses_rebase(cmd, &args)
                    && !old_head.is_empty()
                    && !new_head.is_empty()
                    && old_head != new_head
                {
                    let worktree = cmd
                        .worktree
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string());
                    let (original_commits, new_commits) = build_pull_rebase_mappings_best_effort(
                        worktree.as_deref(),
                        &old_head,
                        &new_head,
                    );
                    let original_commits = if original_commits.is_empty() {
                        vec![old_head.clone()]
                    } else {
                        original_commits
                    };
                    Some(RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
                        old_head,
                        new_head,
                        false,
                        original_commits,
                        new_commits,
                    )))
                } else {
                    None
                }
            }
            "cherry-pick" => {
                if args.iter().any(|arg| arg == "--abort") {
                    let original = if old_head.is_empty() {
                        new_head
                    } else {
                        old_head
                    };
                    if let Some(worktree) = cmd.worktree.as_ref() {
                        let _ =
                            self.clear_cherry_pick_progress(worktree.to_string_lossy().as_ref());
                    }
                    if original.is_empty() {
                        None
                    } else {
                        Some(RewriteLogEvent::cherry_pick_abort(
                            CherryPickAbortEvent::new(original),
                        ))
                    }
                } else if !old_head.is_empty() && !new_head.is_empty() && old_head != new_head {
                    let worktree = cmd
                        .worktree
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string());
                    let is_continue = args.iter().any(|arg| arg == "--continue");
                    let tracked_progress = worktree
                        .as_deref()
                        .and_then(|path| self.cherry_pick_progress(path).ok().flatten());
                    let mut source_commits = tracked_progress
                        .as_ref()
                        .map(|state| state.source_commits.clone())
                        .unwrap_or_default();
                    if source_commits.is_empty() {
                        let source_specs = cherry_pick_source_specs_from_argv(&cmd.raw_argv);
                        source_commits =
                            resolve_commit_specs_to_oids(worktree.as_deref(), &source_specs);
                    }
                    if source_commits.is_empty()
                        && is_valid_oid(&old_head)
                        && !is_zero_oid(&old_head)
                    {
                        source_commits.push(
                            tracked_progress
                                .as_ref()
                                .map(|state| state.original_head.clone())
                                .unwrap_or_else(|| old_head.clone()),
                        );
                    }

                    let event_old_head = tracked_progress
                        .as_ref()
                        .map(|state| state.original_head.clone())
                        .filter(|head| !head.is_empty())
                        .unwrap_or_else(|| old_head.clone());

                    let mut new_commits = cherry_pick_created_commits_best_effort(
                        worktree.as_deref(),
                        &event_old_head,
                        &new_head,
                    );
                    if new_commits.is_empty() {
                        new_commits.push(new_head.clone());
                    }
                    let (source_commits, new_commits) =
                        align_cherry_pick_commits(source_commits, new_commits);
                    if is_continue && let Some(worktree) = worktree.as_deref() {
                        let _ = self.clear_cherry_pick_progress(worktree);
                    }

                    Some(RewriteLogEvent::cherry_pick_complete(
                        CherryPickCompleteEvent::new(
                            event_old_head,
                            new_head,
                            source_commits,
                            new_commits,
                        ),
                    ))
                } else {
                    None
                }
            }
            "merge" => {
                if args.iter().any(|arg| arg == "--squash") {
                    let parsed = parse_git_cli_args(Self::args_without_git(&cmd.raw_argv));
                    let base_branch = cmd
                        .pre_repo
                        .as_ref()
                        .and_then(|repo| repo.branch.clone())
                        .unwrap_or_else(|| "HEAD".to_string());
                    let base_head = cmd
                        .pre_repo
                        .as_ref()
                        .and_then(|repo| repo.head.clone())
                        .filter(|head| is_valid_oid(head) && !is_zero_oid(head))
                        .or_else(|| {
                            cmd.post_repo
                                .as_ref()
                                .and_then(|repo| repo.head.clone())
                                .filter(|head| is_valid_oid(head) && !is_zero_oid(head))
                        })
                        .or_else(|| {
                            if is_valid_oid(&old_head) && !is_zero_oid(&old_head) {
                                Some(old_head.clone())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default();
                    let source_branch = parsed
                        .pos_command(0)
                        .or_else(|| args.last().cloned())
                        .unwrap_or_default();
                    let worktree = cmd
                        .worktree
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string());
                    let source_head = resolve_squash_source_head(worktree.as_deref(), &parsed)
                        .unwrap_or_default();
                    if source_head.is_empty() {
                        debug_log(&format!(
                            "daemon: merge --squash source head unresolved (worktree={}, args={:?})",
                            worktree.as_deref().unwrap_or("<none>"),
                            args
                        ));
                        return None;
                    }
                    if base_head.is_empty() {
                        debug_log(&format!(
                            "daemon: merge --squash base head unresolved (worktree={}, args={:?})",
                            worktree.as_deref().unwrap_or("<none>"),
                            args
                        ));
                        return None;
                    }
                    Some(RewriteLogEvent::merge_squash(MergeSquashEvent::new(
                        source_branch,
                        source_head,
                        base_branch,
                        base_head,
                    )))
                } else {
                    None
                }
            }
            "stash" => {
                let operation = Self::infer_stash_operation(&args);
                let stash_ref = Self::stash_ref_for_event(&operation, &args);
                let stash_sha =
                    Self::resolve_stash_sha_for_event(cmd, &operation, stash_ref.as_deref());
                Some(RewriteLogEvent::stash(StashEvent::new(
                    operation,
                    stash_ref,
                    stash_sha,
                    true,
                    Vec::new(),
                )))
            }
            _ => None,
        }
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

    async fn maybe_apply_side_effects_for_command(
        &self,
        family: Option<&str>,
        cmd: &crate::daemon::domain::NormalizedCommand,
    ) {
        self.observe_cherry_pick_progress(cmd);
        self.observe_rebase_progress(cmd);
        let command_name = Self::command_name_for_side_effects(cmd).unwrap_or_default();
        if !self.mode.apply_side_effects() || cmd.wrapper_mirror || cmd.exit_code != 0 {
            if let Some(family) = family
                && command_name == "pull"
                && !cmd.ref_changes.is_empty()
            {
                let _ = self.append_rewrite_event_for_family(
                    family,
                    json!({
                        "ref_reconcile": {
                            "command": "pull",
                            "ref_changes": cmd.ref_changes,
                        }
                    }),
                );
            }
            return;
        }

        if std::env::var("GIT_AI_DEBUG_DAEMON_TRACE")
            .ok()
            .as_deref()
            .is_some_and(|v| v == "1")
        {
            debug_log(&format!(
                "daemon side-effect command={} seq_hint={} argv={:?} ref_changes={} pre_head={:?} post_head={:?}",
                command_name,
                cmd.root_sid,
                cmd.raw_argv,
                cmd.ref_changes.len(),
                cmd.pre_repo.as_ref().and_then(|repo| repo.head.clone()),
                cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()),
            ));
        }
        if let Some(worktree) = cmd.worktree.as_ref() {
            let worktree = worktree.to_string_lossy().to_string();
            match command_name.as_str() {
                "clone" => {
                    let _ = apply_clone_notes_sync_side_effect(&worktree);
                }
                "fetch" => {
                    let _ = apply_fetch_notes_sync_side_effect(&worktree, &cmd.raw_argv);
                }
                "pull" => {
                    let _ = apply_fetch_notes_sync_side_effect(&worktree, &cmd.raw_argv);
                }
                "push" => {
                    let _ = apply_push_side_effect(&worktree, &cmd.raw_argv);
                }
                _ => {}
            }
        }

        if let Some(event) = self.rewrite_event_from_command(cmd) {
            if let Some(worktree) = cmd.worktree.as_ref() {
                let worktree = worktree.to_string_lossy().to_string();
                let env_overrides = self.env_overrides_for_worktree(worktree.as_ref()).await;
                let _ = apply_rewrite_side_effect(&worktree, event.clone(), env_overrides.as_ref());
            }
            if let Some(family) = family {
                let _ = self.append_rewrite_event_for_family(
                    family,
                    serde_json::to_value(event).unwrap_or_else(|_| json!({"rewrite_error": true})),
                );
            }
        } else if let Some(family) = family
            && command_name == "pull"
        {
            let (old_head, new_head) = Self::resolve_heads_for_command(cmd);
            let has_head_delta =
                !old_head.is_empty() && !new_head.is_empty() && old_head != new_head;
            if !cmd.ref_changes.is_empty() || has_head_delta {
                let _ = self.append_rewrite_event_for_family(
                    family,
                    json!({
                        "ref_reconcile": {
                            "command": "pull",
                            "ref_changes": cmd.ref_changes,
                            "old_head": old_head,
                            "new_head": new_head,
                        }
                    }),
                );
            }
        }

        if command_name == "pull"
            && !Self::pull_uses_rebase(cmd, &Self::command_args_for_side_effects(cmd, "pull"))
            && let Some(worktree) = cmd.worktree.as_ref()
        {
            let (old_head, new_head) = Self::resolve_heads_for_command(cmd);
            if !old_head.is_empty()
                && !new_head.is_empty()
                && old_head != new_head
                && let Ok(repo) = find_repository_in_path(&worktree.to_string_lossy())
                && repo_is_ancestor(&repo, &old_head, &new_head)
            {
                let _ = apply_pull_fast_forward_working_log_side_effect(
                    &worktree.to_string_lossy(),
                    &old_head,
                    &new_head,
                );
            }
        }
    }

    async fn ingest_trace_payload(
        &self,
        payload: Value,
        wait: bool,
    ) -> Result<ControlResponse, GitAiError> {
        if !is_relevant_trace_payload(&payload) {
            return Ok(ControlResponse::ok(
                None,
                None,
                Some(json!({ "ignored": true })),
            ));
        }

        let hinted_family = self.resolve_family_from_payload(&payload);
        if let Some(family) = hinted_family.as_deref() {
            let _ = self.bump_latest_family_seq(family)?;
        }

        let emitted = {
            let mut normalizer = self.normalizer.lock().await;
            normalizer.ingest_payload(&payload)?
        };

        let Some(command) = emitted else {
            return Ok(ControlResponse::ok(
                None,
                None,
                Some(json!({ "buffered": true })),
            ));
        };

        let family_key = command.family_key.as_ref().map(|key| key.0.clone());
        let worktree = command.worktree.clone();
        let ack = self.coordinator.route_command(command.clone()).await?;
        if let Some(family) = family_key.as_deref() {
            self.update_latest_family_seq(family, ack.seq)?;
            self.enqueue_ordered_family_side_effect(family, ack.seq, Some(command.clone()))
                .await?;
        }

        if wait && let Some(worktree) = worktree.as_ref() {
            let _ = self.coordinator.barrier_family(worktree, ack.seq).await;
        }

        Ok(ControlResponse::ok(
            Some(ack.seq),
            if wait { Some(ack.seq) } else { None },
            None,
        ))
    }

    async fn ingest_checkpoint_payload(
        &self,
        repo_working_dir: String,
        payload: Value,
        wait: bool,
    ) -> Result<ControlResponse, GitAiError> {
        let family = self.backend.resolve_family(Path::new(&repo_working_dir))?;
        let id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("cp-{}", short_hash_json(&payload)));
        let author = payload
            .get("author")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        let observed = crate::daemon::domain::CheckpointObserved {
            repo_working_dir: PathBuf::from(&repo_working_dir),
            id,
            author,
            timestamp_ns: now_unix_nanos(),
            file_count: 0,
        };
        let ack = self.coordinator.apply_checkpoint(observed).await?;
        self.update_latest_family_seq(&family.0, ack.seq)?;
        self.enqueue_ordered_family_side_effect(&family.0, ack.seq, None)
            .await?;

        if self.mode.apply_side_effects() {
            let _ = apply_checkpoint_side_effect(&payload);
        }

        if wait {
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
        self.update_latest_family_seq(&family.0, ack.seq)?;
        self.enqueue_ordered_family_side_effect(&family.0, ack.seq, None)
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
        let latest_seq = self.latest_family_seq(&family.0)?.max(status.applied_seq);
        let (pending_roots, deferred_root_exits) = self.pending_counts().await?;
        let active_connections = self.active_trace_connection_count();
        let pending_total = pending_roots
            .saturating_add(deferred_root_exits)
            .saturating_add(active_connections);
        let cursor = latest_seq.saturating_sub(pending_total);
        let backlog = latest_seq.saturating_sub(cursor);
        let inflight_effects = self.inflight_effect_depth(&family.0)?;
        Ok(FamilyStatus {
            family_key: family.0,
            mode: self.mode,
            latest_seq,
            cursor,
            backlog,
            effect_queue_depth: status.effect_queue_depth.saturating_add(inflight_effects),
            active_trace_connections: active_connections as usize,
            unresolved_transcripts: Vec::new(),
            pending_roots: pending_roots as usize,
            deferred_root_exits: deferred_root_exits as usize,
            last_error: status.last_error,
            last_reconcile_ns: status.last_reconcile_ns,
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
        let latest_seq = self.latest_family_seq(&family.0)?.max(snapshot.applied_seq);
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
                    "unresolved_transcripts": snapshot.unresolved_transcripts,
                    "last_error": snapshot.last_error,
                    "last_reconcile_ns": snapshot.last_reconcile_ns,
                }
            })),
        ))
    }

    async fn wait_through_seq(
        &self,
        repo_working_dir: String,
        seq: u64,
    ) -> Result<ControlResponse, GitAiError> {
        let family = self.backend.resolve_family(Path::new(&repo_working_dir))?;
        for _ in 0..400 {
            let latest = self.latest_family_seq(&family.0)?;
            let (pending_roots, deferred_root_exits) = self.pending_counts().await?;
            if latest >= seq
                && pending_roots == 0
                && deferred_root_exits == 0
                && self.active_trace_connection_count() == 0
            {
                return Ok(ControlResponse::ok(Some(seq), Some(latest), None));
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let latest = self.latest_family_seq(&family.0)?;
        Ok(ControlResponse::ok(Some(seq), Some(latest), None))
    }

    async fn reconcile_family(
        &self,
        repo_working_dir: String,
    ) -> Result<ControlResponse, GitAiError> {
        let family = self.backend.resolve_family(Path::new(&repo_working_dir))?;
        let refs = self.backend.ref_snapshot(&family)?;
        let ack = self
            .coordinator
            .reconcile_family(
                Path::new(&repo_working_dir),
                crate::daemon::domain::ReconcileSnapshot {
                    refs,
                    timestamp_ns: now_unix_nanos(),
                },
            )
            .await?;
        self.update_latest_family_seq(&family.0, ack.seq)?;
        self.enqueue_ordered_family_side_effect(&family.0, ack.seq, None)
            .await?;
        Ok(ControlResponse::ok(Some(ack.seq), Some(ack.seq), None))
    }

    async fn handle_control_request(&self, request: ControlRequest) -> ControlResponse {
        let result = match request {
            ControlRequest::TraceIngest {
                repo_working_dir,
                payload,
                wait,
            } => {
                let _ = repo_working_dir;
                self.ingest_trace_payload(payload, wait.unwrap_or(false))
                    .await
            }
            ControlRequest::CheckpointRun {
                repo_working_dir,
                payload,
                wait,
            } => {
                self.ingest_checkpoint_payload(repo_working_dir, payload, wait.unwrap_or(false))
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
            ControlRequest::ReconcileFamily { repo_working_dir } => {
                self.reconcile_family(repo_working_dir).await
            }
            ControlRequest::Shutdown => {
                self.request_shutdown();
                Ok(ControlResponse::ok(None, None, None))
            }
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
        let response = match parsed {
            Ok(req) => {
                runtime_handle.block_on(async { coordinator.handle_control_request(req).await })
            }
            Err(e) => ControlResponse::err(format!("invalid control request: {}", e)),
        };
        let raw = serde_json::to_string(&response)?;
        reader.get_mut().write_all(raw.as_bytes())?;
        reader.get_mut().write_all(b"\n")?;
        reader.get_mut().flush()?;
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
    while let Some(line) = read_json_line(&mut reader)? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let _ = runtime_handle
            .block_on(async { coordinator.ingest_trace_payload(parsed, false).await });
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

    let coordinator = Arc::new(ActorDaemonCoordinator::new(config.mode));
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
