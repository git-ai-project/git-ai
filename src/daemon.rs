use crate::config;
use crate::error::GitAiError;
use crate::git::cli_parser::{ParsedGitInvocation, parse_git_cli_args};
use crate::git::repository::{Repository, exec_git};
use crate::git::rewrite_log::{
    CherryPickAbortEvent, CherryPickCompleteEvent, MergeSquashEvent, RebaseAbortEvent,
    RebaseCompleteEvent, ResetEvent, ResetKind, RewriteLogEvent, StashEvent, StashOperation,
};
use crate::git::{find_repository, find_repository_in_path, from_bare_repository};
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
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc};

const DAEMON_STATE_VERSION: &str = "daemon-v1";
const TRACE_EVENT_TYPE: &str = "trace2_raw";
const CHECKPOINT_EVENT_TYPE: &str = "checkpoint";
const RECONCILE_EVENT_TYPE: &str = "reconcile";
const ENV_OVERRIDE_EVENT_TYPE: &str = "env_override";
const COMMAND_INDEX_FILE: &str = "command_index.jsonl";
const CHECKPOINT_INDEX_FILE: &str = "checkpoint_index.jsonl";
const PID_META_FILE: &str = "daemon.pid.json";
const REANCHOR_IDLE_NS: u128 = 5_000_000_000;

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
pub struct EventEnvelope {
    pub seq: u64,
    pub repo_family: String,
    pub source: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub received_at_ns: u128,
    pub payload: Value,
    pub checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepoSnapshot {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub refs: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PendingRootCommand {
    pub sid: String,
    pub start_seq: u64,
    pub start_ns: u128,
    pub argv: Vec<String>,
    pub name: Option<String>,
    pub worktree: Option<String>,
    pub pre_snapshot: Option<RepoSnapshot>,
    #[serde(default)]
    pub wrapper_mirror: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeferredRootExit {
    pub seq: u64,
    pub received_at_ns: u128,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppliedCommand {
    pub seq: u64,
    pub sid: String,
    pub name: String,
    pub argv: Vec<String>,
    pub exit_code: i32,
    pub worktree: Option<String>,
    pub pre_head: Option<String>,
    pub post_head: Option<String>,
    pub ref_changes: Vec<RefChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CheckpointSummary {
    pub kind: Option<String>,
    pub author: Option<String>,
    pub agent_id: Option<Value>,
    pub entries_hash: String,
    pub transcript_hash: Option<String>,
    pub line_stats: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RefChange {
    pub reference: String,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ActiveCherryPickState {
    pub original_head: String,
    pub source_commits: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReflogCursorState {
    pub path: String,
    pub reference: String,
    pub offset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReflogAnchorState {
    pub at_seq: u64,
    pub anchored_at_ns: u128,
    pub cursors: Vec<ReflogCursorState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FamilyState {
    pub pending_roots: HashMap<String, PendingRootCommand>,
    #[serde(default)]
    pub deferred_root_exits: HashMap<String, DeferredRootExit>,
    #[serde(default)]
    pub sid_worktrees: HashMap<String, String>,
    #[serde(default)]
    pub worktree_snapshots: HashMap<String, RepoSnapshot>,
    pub commands: Vec<AppliedCommand>,
    pub checkpoints: HashMap<String, CheckpointSummary>,
    pub unresolved_transcripts: BTreeSet<String>,
    pub rewrite_events: Vec<Value>,
    #[serde(default)]
    pub active_cherry_pick_by_worktree: HashMap<String, ActiveCherryPickState>,
    #[serde(default)]
    pub env_overrides_by_worktree: HashMap<String, HashMap<String, String>>,
    pub last_snapshot: RepoSnapshot,
    pub dedupe_trace: BTreeSet<String>,
    pub dedupe_checkpoints: BTreeSet<String>,
    pub last_error: Option<String>,
    pub last_reconcile_ns: Option<u128>,
    #[serde(default)]
    pub reflog_anchor: Option<ReflogAnchorState>,
    #[serde(default)]
    pub reflog_drifted: bool,
    #[serde(default)]
    pub last_event_applied_ns: Option<u128>,
    #[serde(default)]
    pub last_reanchor_ns: Option<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CursorState {
    cursor: u64,
}

#[derive(Debug, Clone)]
pub struct FamilyStore {
    pub common_dir: PathBuf,
    pub root: PathBuf,
    pub events_file: PathBuf,
    pub seq_file: PathBuf,
    pub cursor_file: PathBuf,
    pub state_file: PathBuf,
    pub command_index_file: PathBuf,
    pub checkpoint_index_file: PathBuf,
    pub reconcile_dir: PathBuf,
}

impl FamilyStore {
    pub fn for_common_dir(common_dir: &Path) -> Result<Self, GitAiError> {
        let root = common_dir
            .join("ai")
            .join("state")
            .join(DAEMON_STATE_VERSION)
            .to_path_buf();
        let events_dir = root.join("events");
        fs::create_dir_all(&events_dir)?;
        let events_file = events_dir.join("00000001.jsonl");
        if !events_file.exists() {
            fs::write(&events_file, "")?;
        }
        let seq_file = root.join("seq");
        if !seq_file.exists() {
            fs::write(&seq_file, "0\n")?;
        }
        let cursor_file = root.join("cursor.json");
        if !cursor_file.exists() {
            fs::write(
                &cursor_file,
                serde_json::to_string_pretty(&CursorState { cursor: 0 })?,
            )?;
        }
        let state_file = root.join("family_state.json");
        if !state_file.exists() {
            fs::write(
                &state_file,
                serde_json::to_string_pretty(&FamilyState::default())?,
            )?;
        }
        let command_index_file = root.join(COMMAND_INDEX_FILE);
        if !command_index_file.exists() {
            fs::write(&command_index_file, "")?;
        }
        let checkpoint_index_file = root.join(CHECKPOINT_INDEX_FILE);
        if !checkpoint_index_file.exists() {
            fs::write(&checkpoint_index_file, "")?;
        }
        let reconcile_dir = root.join("reconcile");
        fs::create_dir_all(&reconcile_dir)?;
        Ok(Self {
            common_dir: common_dir.to_path_buf(),
            root,
            events_file,
            seq_file,
            cursor_file,
            state_file,
            command_index_file,
            checkpoint_index_file,
            reconcile_dir,
        })
    }

    fn read_seq(&self) -> Result<u64, GitAiError> {
        let raw = fs::read_to_string(&self.seq_file)?;
        Ok(raw.trim().parse::<u64>().unwrap_or(0))
    }

    fn write_seq(&self, seq: u64) -> Result<(), GitAiError> {
        fs::write(&self.seq_file, format!("{seq}\n"))?;
        Ok(())
    }

    pub fn append_event(
        &self,
        repo_family: &str,
        source: &str,
        event_type: &str,
        payload: Value,
    ) -> Result<EventEnvelope, GitAiError> {
        let seq = self.read_seq()?.saturating_add(1);
        self.write_seq(seq)?;
        let received_at_ns = now_unix_nanos();
        let checksum = checksum_for(
            seq,
            repo_family,
            source,
            event_type,
            received_at_ns,
            &payload,
        );
        let envelope = EventEnvelope {
            seq,
            repo_family: repo_family.to_string(),
            source: source.to_string(),
            event_type: event_type.to_string(),
            received_at_ns,
            payload,
            checksum,
        };
        let serialized = serde_json::to_string(&envelope)?;
        let mut file = OpenOptions::new().append(true).open(&self.events_file)?;
        file.write_all(serialized.as_bytes())?;
        file.write_all(b"\n")?;
        Ok(envelope)
    }

    pub fn latest_seq(&self) -> Result<u64, GitAiError> {
        self.read_seq()
    }

    pub fn read_events_after(&self, cursor: u64) -> Result<Vec<EventEnvelope>, GitAiError> {
        let file = OpenOptions::new().read(true).open(&self.events_file)?;
        let mut out = Vec::new();
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let event: EventEnvelope = serde_json::from_str(&line)?;
            if event.seq > cursor {
                out.push(event);
            }
        }
        out.sort_by_key(|e| e.seq);
        Ok(out)
    }

    pub fn load_cursor(&self) -> Result<u64, GitAiError> {
        let content = fs::read_to_string(&self.cursor_file)?;
        let parsed: CursorState = serde_json::from_str(&content).unwrap_or_default();
        Ok(parsed.cursor)
    }

    pub fn save_cursor(&self, cursor: u64) -> Result<(), GitAiError> {
        let content = serde_json::to_string_pretty(&CursorState { cursor })?;
        fs::write(&self.cursor_file, content)?;
        Ok(())
    }

    pub fn load_state(&self) -> Result<FamilyState, GitAiError> {
        let content = fs::read_to_string(&self.state_file)?;
        Ok(serde_json::from_str(&content).unwrap_or_default())
    }

    pub fn save_state(&self, state: &FamilyState) -> Result<(), GitAiError> {
        let content = serde_json::to_string_pretty(state)?;
        fs::write(&self.state_file, content)?;
        Ok(())
    }

    pub fn append_command_index(&self, command: &AppliedCommand) -> Result<(), GitAiError> {
        append_jsonl(&self.command_index_file, command)
    }

    pub fn append_checkpoint_index(
        &self,
        checkpoint_id: &str,
        summary: &CheckpointSummary,
    ) -> Result<(), GitAiError> {
        append_jsonl(
            &self.checkpoint_index_file,
            &json!({
                "checkpoint_id": checkpoint_id,
                "summary": summary
            }),
        )
    }

    pub fn append_reconcile_record(&self, record: &Value) -> Result<(), GitAiError> {
        let filename = format!("{}.jsonl", now_unix_nanos());
        let path = self.reconcile_dir.join(filename);
        append_jsonl(&path, record)
    }
}

#[derive(Debug)]
struct FamilyRuntime {
    store: FamilyStore,
    mode: DaemonMode,
    append_lock: AsyncMutex<()>,
    notify_tx: mpsc::UnboundedSender<()>,
    applied_seq: AtomicU64,
    applied_notify: Notify,
}

impl FamilyRuntime {
    async fn wait_for_applied(&self, seq: u64) {
        while self.applied_seq.load(Ordering::SeqCst) < seq {
            self.applied_notify.notified().await;
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
    pub unresolved_transcripts: Vec<String>,
    pub pending_roots: usize,
    pub deferred_root_exits: usize,
    pub last_error: Option<String>,
    pub last_reconcile_ns: Option<u128>,
}

#[derive(Debug)]
pub struct DaemonCoordinator {
    mode: DaemonMode,
    shutdown: AtomicBool,
    shutdown_notify: Notify,
    family_map: Mutex<HashMap<String, Arc<FamilyRuntime>>>,
    sid_family_map: Mutex<HashMap<String, String>>,
    pending_trace_by_root: Mutex<HashMap<String, Vec<Value>>>,
}

impl DaemonCoordinator {
    pub fn new(config: DaemonConfig) -> Self {
        Self {
            mode: config.mode,
            shutdown: AtomicBool::new(false),
            shutdown_notify: Notify::new(),
            family_map: Mutex::new(HashMap::new()),
            sid_family_map: Mutex::new(HashMap::new()),
            pending_trace_by_root: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
    }

    pub async fn wait_for_shutdown(&self) {
        if self.is_shutting_down() {
            return;
        }
        self.shutdown_notify.notified().await;
    }

    async fn get_or_create_family_runtime(
        self: &Arc<Self>,
        family_key: String,
        common_dir: PathBuf,
    ) -> Result<Arc<FamilyRuntime>, GitAiError> {
        if let Some(existing) = self
            .family_map
            .lock()
            .map_err(|_| GitAiError::Generic("family map lock poisoned".to_string()))?
            .get(&family_key)
            .cloned()
        {
            return Ok(existing);
        }

        let store = FamilyStore::for_common_dir(&common_dir)?;
        let (notify_tx, notify_rx) = mpsc::unbounded_channel();
        let runtime = Arc::new(FamilyRuntime {
            store,
            mode: self.mode,
            append_lock: AsyncMutex::new(()),
            notify_tx,
            applied_seq: AtomicU64::new(0),
            applied_notify: Notify::new(),
        });

        {
            let mut map = self
                .family_map
                .lock()
                .map_err(|_| GitAiError::Generic("family map lock poisoned".to_string()))?;
            map.entry(family_key.clone())
                .or_insert_with(|| runtime.clone());
        }

        let runtime_for_task = runtime.clone();
        tokio::spawn(async move {
            if let Err(e) = family_worker_loop(runtime_for_task, notify_rx).await {
                debug_log(&format!("daemon family worker failed: {}", e));
            }
        });

        Ok(runtime)
    }

    fn root_sid(sid: &str) -> &str {
        sid.split('/').next().unwrap_or(sid)
    }

    fn resolve_family_from_worktree(
        &self,
        worktree: &str,
    ) -> Result<(String, PathBuf), GitAiError> {
        let repo = find_repository_in_path(worktree)?;
        let common_dir = repo
            .common_dir()
            .canonicalize()
            .unwrap_or_else(|_| repo.common_dir().to_path_buf());
        let family_key = common_dir.to_string_lossy().to_string();
        Ok((family_key, common_dir))
    }

    fn resolve_family_for_trace_payload(
        &self,
        payload: &Value,
    ) -> Result<Option<(String, PathBuf)>, GitAiError> {
        let sid = payload.get("sid").and_then(Value::as_str);
        if let Some(worktree) = payload
            .get("worktree")
            .and_then(Value::as_str)
            .or_else(|| payload.get("repo_working_dir").and_then(Value::as_str))
        {
            let resolved = self.resolve_family_from_worktree(worktree)?;
            if let Some(raw_sid) = sid {
                let root = Self::root_sid(raw_sid).to_string();
                self.sid_family_map
                    .lock()
                    .map_err(|_| GitAiError::Generic("sid map lock poisoned".to_string()))?
                    .insert(root, resolved.0.clone());
            }
            return Ok(Some(resolved));
        }

        if let Some(raw_sid) = sid {
            let root = Self::root_sid(raw_sid).to_string();
            if let Some(family_key) = self
                .sid_family_map
                .lock()
                .map_err(|_| GitAiError::Generic("sid map lock poisoned".to_string()))?
                .get(&root)
                .cloned()
            {
                return Ok(Some((family_key.clone(), PathBuf::from(family_key))));
            }
        }
        Ok(None)
    }

    async fn append_family_event(
        self: &Arc<Self>,
        family_key: String,
        common_dir: PathBuf,
        source: &str,
        event_type: &str,
        payload: Value,
    ) -> Result<(u64, Arc<FamilyRuntime>), GitAiError> {
        let runtime = self
            .get_or_create_family_runtime(family_key.clone(), common_dir)
            .await?;
        let _guard = runtime.append_lock.lock().await;
        let event = runtime
            .store
            .append_event(&family_key, source, event_type, payload)?;
        drop(_guard);
        let _ = runtime.notify_tx.send(());
        Ok((event.seq, runtime))
    }

    pub async fn ingest_trace_payload(
        self: &Arc<Self>,
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
        let payload = payload;
        let root_sid = payload
            .get("sid")
            .and_then(Value::as_str)
            .map(Self::root_sid)
            .map(ToString::to_string);
        let Some((family_key, common_dir)) = self.resolve_family_for_trace_payload(&payload)?
        else {
            if let Some(root_sid) = root_sid {
                let mut pending_map = self.pending_trace_by_root.lock().map_err(|_| {
                    GitAiError::Generic("pending trace map lock poisoned".to_string())
                })?;
                let entry = pending_map.entry(root_sid).or_insert_with(Vec::new);
                // Keep memory bounded if a sid never resolves to a family.
                if entry.len() >= 512 {
                    entry.remove(0);
                }
                entry.push(payload);
                return Ok(ControlResponse::ok(
                    None,
                    None,
                    Some(json!({ "buffered": true })),
                ));
            }
            return Ok(ControlResponse::err(
                "trace payload missing resolvable worktree/family",
            ));
        };

        if let Some(root_sid) = root_sid.as_deref() {
            let resolved_worktree = payload
                .get("worktree")
                .and_then(Value::as_str)
                .or_else(|| payload.get("repo_working_dir").and_then(Value::as_str))
                .map(ToString::to_string);
            let mut pending = self
                .pending_trace_by_root
                .lock()
                .map_err(|_| GitAiError::Generic("pending trace map lock poisoned".to_string()))?
                .remove(root_sid)
                .unwrap_or_default();
            pending.sort_by(compare_buffered_trace_payloads);
            for mut buffered_payload in pending {
                if let Some(worktree) = resolved_worktree.as_ref() {
                    let missing_worktree = buffered_payload.get("worktree").is_none()
                        && buffered_payload.get("repo_working_dir").is_none();
                    if missing_worktree && let Some(obj) = buffered_payload.as_object_mut() {
                        obj.insert("worktree".to_string(), Value::String(worktree.clone()));
                        obj.insert(
                            "repo_working_dir".to_string(),
                            Value::String(worktree.clone()),
                        );
                    }
                }
                let _ = self
                    .append_family_event(
                        family_key.clone(),
                        common_dir.clone(),
                        "trace2",
                        TRACE_EVENT_TYPE,
                        buffered_payload,
                    )
                    .await?;
            }
        }

        let (seq, runtime) = self
            .append_family_event(family_key, common_dir, "trace2", TRACE_EVENT_TYPE, payload)
            .await?;

        if wait {
            runtime.wait_for_applied(seq).await;
            let applied = runtime.applied_seq.load(Ordering::SeqCst);
            return Ok(ControlResponse::ok(Some(seq), Some(applied), None));
        }
        Ok(ControlResponse::ok(Some(seq), None, None))
    }

    pub async fn ingest_checkpoint_payload(
        self: &Arc<Self>,
        repo_working_dir: String,
        mut payload: Value,
        wait: bool,
    ) -> Result<ControlResponse, GitAiError> {
        let (family_key, common_dir) = self.resolve_family_from_worktree(&repo_working_dir)?;
        if payload.get("repo_working_dir").is_none() {
            payload["repo_working_dir"] = Value::String(repo_working_dir);
        }
        let (seq, runtime) = self
            .append_family_event(
                family_key,
                common_dir,
                "checkpoint",
                CHECKPOINT_EVENT_TYPE,
                payload,
            )
            .await?;
        if wait {
            runtime.wait_for_applied(seq).await;
            let applied = runtime.applied_seq.load(Ordering::SeqCst);
            return Ok(ControlResponse::ok(Some(seq), Some(applied), None));
        }
        Ok(ControlResponse::ok(Some(seq), None, None))
    }

    pub async fn ingest_env_override(
        self: &Arc<Self>,
        repo_working_dir: String,
        env: HashMap<String, String>,
        wait: bool,
    ) -> Result<ControlResponse, GitAiError> {
        let (family_key, common_dir) = self.resolve_family_from_worktree(&repo_working_dir)?;
        let payload = json!({
            "repo_working_dir": repo_working_dir,
            "env": env,
        });
        let (seq, runtime) = self
            .append_family_event(
                family_key,
                common_dir,
                "control",
                ENV_OVERRIDE_EVENT_TYPE,
                payload,
            )
            .await?;
        if wait {
            runtime.wait_for_applied(seq).await;
            let applied = runtime.applied_seq.load(Ordering::SeqCst);
            return Ok(ControlResponse::ok(Some(seq), Some(applied), None));
        }
        Ok(ControlResponse::ok(Some(seq), None, None))
    }

    pub async fn status_for_family(
        self: &Arc<Self>,
        repo_working_dir: String,
    ) -> Result<FamilyStatus, GitAiError> {
        let (family_key, common_dir) = self.resolve_family_from_worktree(&repo_working_dir)?;
        let store = FamilyStore::for_common_dir(&common_dir)?;
        let latest_seq = store.latest_seq()?;
        let cursor = store.load_cursor()?;
        let state = store.load_state()?;
        let event_backlog = latest_seq.saturating_sub(cursor);
        let live_backlog =
            state.pending_roots.len() as u64 + state.deferred_root_exits.len() as u64;
        let backlog = event_backlog.saturating_add(live_backlog);
        Ok(FamilyStatus {
            family_key,
            mode: self.mode,
            latest_seq,
            cursor,
            backlog,
            unresolved_transcripts: state.unresolved_transcripts.iter().cloned().collect(),
            pending_roots: state.pending_roots.len(),
            deferred_root_exits: state.deferred_root_exits.len(),
            last_error: state.last_error,
            last_reconcile_ns: state.last_reconcile_ns,
        })
    }

    pub async fn wait_through_seq(
        self: &Arc<Self>,
        repo_working_dir: String,
        seq: u64,
    ) -> Result<ControlResponse, GitAiError> {
        let (family_key, common_dir) = self.resolve_family_from_worktree(&repo_working_dir)?;
        let runtime = self
            .get_or_create_family_runtime(family_key, common_dir)
            .await?;
        runtime.wait_for_applied(seq).await;
        let applied = runtime.applied_seq.load(Ordering::SeqCst);
        Ok(ControlResponse::ok(Some(seq), Some(applied), None))
    }

    pub async fn reconcile_family(
        self: &Arc<Self>,
        repo_working_dir: String,
    ) -> Result<ControlResponse, GitAiError> {
        let (family_key, common_dir) = self.resolve_family_from_worktree(&repo_working_dir)?;
        let payload = json!({
            "reason": "manual",
            "repo_working_dir": repo_working_dir
        });
        let (seq, runtime) = self
            .append_family_event(
                family_key,
                common_dir,
                "control",
                RECONCILE_EVENT_TYPE,
                payload,
            )
            .await?;
        runtime.wait_for_applied(seq).await;
        let applied = runtime.applied_seq.load(Ordering::SeqCst);
        Ok(ControlResponse::ok(Some(seq), Some(applied), None))
    }

    pub async fn handle_control_request(
        self: &Arc<Self>,
        request: ControlRequest,
    ) -> ControlResponse {
        let result = match request {
            ControlRequest::TraceIngest {
                repo_working_dir,
                mut payload,
                wait,
            } => {
                if payload.get("repo_working_dir").is_none() {
                    payload["repo_working_dir"] = Value::String(repo_working_dir);
                }
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
            Ok(resp) => resp,
            Err(e) => ControlResponse::err(e.to_string()),
        }
    }
}

async fn family_worker_loop(
    runtime: Arc<FamilyRuntime>,
    mut notify_rx: mpsc::UnboundedReceiver<()>,
) -> Result<(), GitAiError> {
    let mut cursor = runtime.store.load_cursor()?;
    runtime.applied_seq.store(cursor, Ordering::SeqCst);
    let mut state = runtime.store.load_state()?;
    if state.last_snapshot.refs.is_empty()
        && let Ok(snapshot) = snapshot_common_dir(&runtime.store.common_dir)
    {
        state.last_snapshot = snapshot;
    }
    let _ = ensure_reflog_anchor(runtime.as_ref(), &mut state, cursor, false);
    runtime.store.save_state(&state)?;

    loop {
        let mut progressed = false;
        let events = runtime.store.read_events_after(cursor)?;
        if !events.is_empty() {
            for event in events.into_iter().take(512) {
                if let Err(e) = verify_event_checksum(&event) {
                    state.last_error = Some(e.to_string());
                    let _ = runtime.store.append_reconcile_record(&json!({
                        "seq": event.seq,
                        "kind": "checksum_error",
                        "error": e.to_string(),
                    }));
                } else if let Err(e) = apply_event(&runtime, &mut state, &event) {
                    state.last_error = Some(e.to_string());
                    let _ = runtime.store.append_reconcile_record(&json!({
                        "seq": event.seq,
                        "kind": "apply_error",
                        "error": e.to_string(),
                    }));
                } else {
                    state.last_error = None;
                }
                cursor = event.seq;
                state.last_event_applied_ns = Some(now_unix_nanos());
                runtime.store.save_state(&state)?;
                runtime.store.save_cursor(cursor)?;
                runtime.applied_seq.store(cursor, Ordering::SeqCst);
                runtime.applied_notify.notify_waiters();
                progressed = true;
            }
        } else {
            match maybe_reanchor_family_state(runtime.as_ref(), &mut state, cursor) {
                Ok(true) => {
                    runtime.store.save_state(&state)?;
                    progressed = true;
                }
                Ok(false) => {}
                Err(e) => {
                    state.last_error = Some(format!("reanchor failed: {}", e));
                    let _ = runtime.store.append_reconcile_record(&json!({
                        "seq": cursor,
                        "kind": "reanchor_error",
                        "error": e.to_string(),
                    }));
                    runtime.store.save_state(&state)?;
                }
            }
        }
        if !progressed {
            match notify_rx.recv().await {
                Some(_) => {}
                None => break,
            }
        }
    }
    Ok(())
}

fn verify_event_checksum(event: &EventEnvelope) -> Result<(), GitAiError> {
    let expected = checksum_for(
        event.seq,
        &event.repo_family,
        &event.source,
        &event.event_type,
        event.received_at_ns,
        &event.payload,
    );
    if expected != event.checksum {
        return Err(GitAiError::Generic(format!(
            "checksum mismatch at seq {}",
            event.seq
        )));
    }
    Ok(())
}

fn apply_event(
    runtime: &FamilyRuntime,
    state: &mut FamilyState,
    event: &EventEnvelope,
) -> Result<(), GitAiError> {
    match event.event_type.as_str() {
        TRACE_EVENT_TYPE => apply_trace_event(runtime, state, event),
        CHECKPOINT_EVENT_TYPE => apply_checkpoint_event(runtime, state, event),
        RECONCILE_EVENT_TYPE => apply_reconcile_event(runtime, state, event),
        ENV_OVERRIDE_EVENT_TYPE => apply_env_override_event(state, event),
        other => Err(GitAiError::Generic(format!(
            "unknown daemon event type: {}",
            other
        ))),
    }
}

fn is_root_sid(sid: &str) -> bool {
    !sid.contains('/')
}

fn root_from_sid(sid: &str) -> String {
    sid.split('/').next().unwrap_or(sid).to_string()
}

fn trace_event_phase_rank(payload: &Value) -> u8 {
    match payload
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "start" => 0,
        "def_repo" => 1,
        "cmd_name" => 2,
        "exit" => 3,
        _ => 4,
    }
}

fn compare_buffered_trace_payloads(left: &Value, right: &Value) -> std::cmp::Ordering {
    let left_rank = trace_event_phase_rank(left);
    let right_rank = trace_event_phase_rank(right);
    if left_rank != right_rank {
        return left_rank.cmp(&right_rank);
    }

    let left_abs = left
        .get("t_abs")
        .and_then(Value::as_f64)
        .unwrap_or(f64::INFINITY);
    let right_abs = right
        .get("t_abs")
        .and_then(Value::as_f64)
        .unwrap_or(f64::INFINITY);
    left_abs
        .partial_cmp(&right_abs)
        .unwrap_or(std::cmp::Ordering::Equal)
}

fn is_relevant_trace_payload(payload: &Value) -> bool {
    let Some(event) = payload.get("event").and_then(Value::as_str) else {
        return false;
    };
    if event == "def_repo" {
        return true;
    }
    if !matches!(event, "start" | "cmd_name" | "exit") {
        return false;
    }
    payload
        .get("sid")
        .and_then(Value::as_str)
        .map(is_root_sid)
        .unwrap_or(false)
}

fn apply_exit_for_pending_root(
    runtime: &FamilyRuntime,
    state: &mut FamilyState,
    pending: PendingRootCommand,
    exit_seq: u64,
    exit_payload: &Value,
) -> Result<(), GitAiError> {
    let exit_code = exit_payload
        .get("code")
        .and_then(Value::as_i64)
        .unwrap_or(1) as i32;
    let dedupe_key = format!(
        "{}:{}:{}",
        pending.sid,
        pending.start_seq,
        short_hash_json(&json!(pending.argv))
    );
    if state.dedupe_trace.contains(&dedupe_key) {
        return Ok(());
    }
    state.dedupe_trace.insert(dedupe_key);

    let pre_snapshot = pending
        .worktree
        .as_ref()
        .and_then(|worktree| state.worktree_snapshots.get(worktree))
        .cloned()
        .or_else(|| pending.pre_snapshot.clone())
        .unwrap_or_else(|| state.last_snapshot.clone());
    let command_name = pending
        .name
        .as_deref()
        .or_else(|| argv_primary_command(&pending.argv))
        .unwrap_or_default();
    let ref_changes = if command_may_mutate_refs(command_name, &pending.argv) {
        consume_reflog_ref_changes(runtime, state, exit_seq)?
    } else {
        vec![]
    };
    let mut post_snapshot = apply_ref_changes_to_snapshot(&pre_snapshot, &ref_changes);
    if post_snapshot.branch.is_none() {
        post_snapshot.branch = pre_snapshot.branch.clone();
    }
    if post_snapshot.head.is_none() {
        post_snapshot.head = pre_snapshot.head.clone();
    }
    if exit_code == 0
        && let Some(branch) = infer_branch_after_command(
            pending.name.as_deref().unwrap_or_default(),
            &pending.argv,
            pre_snapshot.branch.as_deref(),
        )
    {
        post_snapshot.branch = Some(branch);
        update_snapshot_head_from_branch(&mut post_snapshot);
    }
    if let Some(worktree) = pending.worktree.as_ref() {
        state
            .worktree_snapshots
            .insert(worktree.clone(), post_snapshot.clone());
    }
    state.last_snapshot = post_snapshot.clone();

    let command = AppliedCommand {
        seq: exit_seq,
        sid: pending.sid.clone(),
        name: pending.name.clone().unwrap_or_default(),
        argv: pending.argv.clone(),
        exit_code,
        worktree: pending.worktree.clone(),
        pre_head: pre_snapshot.head.clone(),
        post_head: post_snapshot.head.clone(),
        ref_changes: ref_changes.clone(),
    };
    state.commands.push(command);
    if state.commands.len() > 1000 {
        state.commands.drain(0..state.commands.len() - 1000);
    }
    if let Some(last) = state.commands.last() {
        runtime.store.append_command_index(last)?;
    }

    let cherry_pick_worktree_key = if pending.name.as_deref() == Some("cherry-pick") {
        Some(cherry_pick_worktree_key(pending.worktree.as_deref()))
    } else {
        None
    };
    if pending.name.as_deref() == Some("cherry-pick")
        && !cherry_pick_continue_flag(&pending.argv)
        && !cherry_pick_abort_flag(&pending.argv)
    {
        let source_specs = cherry_pick_source_specs_from_argv(&pending.argv);
        let source_commits =
            resolve_commit_specs_to_oids(pending.worktree.as_deref(), &source_specs);
        let (old_head, _) = resolve_command_heads(&pre_snapshot, &post_snapshot, &ref_changes);
        let original_head = old_head
            .or_else(|| pre_snapshot.head.clone())
            .unwrap_or_default();
        if !source_commits.is_empty()
            && !original_head.is_empty()
            && let Some(key) = cherry_pick_worktree_key.as_ref()
        {
            state.active_cherry_pick_by_worktree.insert(
                key.clone(),
                ActiveCherryPickState {
                    original_head,
                    source_commits,
                },
            );
        }
    }

    if exit_code == 0 {
        if runtime.mode.apply_side_effects() && !pending.wrapper_mirror {
            if pending.name.as_deref() == Some("push")
                && let Some(worktree) = pending.worktree.as_deref()
            {
                let _ = apply_push_side_effect(worktree, &pending.argv);
            }

            if pending.name.as_deref() == Some("pull")
                && !pull_uses_rebase(&pending.argv, pending.worktree.as_deref())
            {
                let (old_head, new_head) =
                    resolve_command_heads(&pre_snapshot, &post_snapshot, &ref_changes);
                if let (Some(old_head), Some(new_head)) = (old_head, new_head)
                    && !old_head.is_empty()
                    && !new_head.is_empty()
                    && old_head != new_head
                {
                    let is_fast_forward = if let Some(worktree) = pending.worktree.as_deref() {
                        find_repository_in_path(worktree)
                            .map(|repo| repo_is_ancestor(&repo, &old_head, &new_head))
                            .unwrap_or(false)
                    } else {
                        from_bare_repository(&runtime.store.common_dir)
                            .map(|repo| repo_is_ancestor(&repo, &old_head, &new_head))
                            .unwrap_or(false)
                    };

                    if is_fast_forward {
                        if let Some(worktree) = pending.worktree.as_deref() {
                            let _ = apply_pull_fast_forward_working_log_side_effect(
                                worktree, &old_head, &new_head,
                            );
                        } else {
                            let _ = apply_pull_fast_forward_working_log_side_effect_from_common_dir(
                                &runtime.store.common_dir,
                                &old_head,
                                &new_head,
                            );
                        }
                    }
                }
            }
        }

        let active_cherry_pick = cherry_pick_worktree_key
            .as_ref()
            .and_then(|key| state.active_cherry_pick_by_worktree.get(key).cloned());
        let rewrite_event = if should_synthesize_rewrite_from_snapshots(
            pending.name.as_deref().unwrap_or_default(),
            &pending.argv,
            &pre_snapshot,
            &post_snapshot,
        ) {
            synthesize_rewrite_event(
                pending.name.as_deref().unwrap_or_default(),
                &pending.argv,
                &pre_snapshot,
                &post_snapshot,
                &ref_changes,
                pending.worktree.as_deref(),
                active_cherry_pick.as_ref(),
            )
        } else {
            None
        };
        if let Some(rewrite_event) = rewrite_event {
            if matches!(
                rewrite_event,
                RewriteLogEvent::CherryPickComplete { .. }
                    | RewriteLogEvent::CherryPickAbort { .. }
            ) && let Some(key) = cherry_pick_worktree_key.as_ref()
            {
                state.active_cherry_pick_by_worktree.remove(key);
            }
            state
                .rewrite_events
                .push(serde_json::to_value(&rewrite_event)?);
            if state.rewrite_events.len() > 1000 {
                state
                    .rewrite_events
                    .drain(0..state.rewrite_events.len() - 1000);
            }
            if runtime.mode.apply_side_effects() && !pending.wrapper_mirror {
                let env_overrides = pending
                    .worktree
                    .as_ref()
                    .and_then(|worktree| state.env_overrides_by_worktree.get(worktree).cloned());
                if let Some(worktree) = pending.worktree.as_deref() {
                    let _ =
                        apply_rewrite_side_effect(worktree, rewrite_event, env_overrides.as_ref());
                } else {
                    let _ = apply_rewrite_side_effect_from_common_dir(
                        &runtime.store.common_dir,
                        rewrite_event,
                        env_overrides.as_ref(),
                    );
                }
            }
        } else if !ref_changes.is_empty()
            && matches!(
                pending.name.as_deref(),
                Some("update-ref")
                    | Some("commit-tree")
                    | Some("pull")
                    | Some("checkout")
                    | Some("switch")
            )
        {
            state.rewrite_events.push(json!({
                "ref_reconcile": {
                    "command": pending.name.clone().unwrap_or_default(),
                    "ref_changes": ref_changes
                }
            }));
        }
    } else if pending.name.as_deref() == Some("cherry-pick")
        && cherry_pick_abort_flag(&pending.argv)
        && let Some(key) = cherry_pick_worktree_key.as_ref()
    {
        state.active_cherry_pick_by_worktree.remove(key);
    }

    Ok(())
}

fn apply_trace_event(
    runtime: &FamilyRuntime,
    state: &mut FamilyState,
    event: &EventEnvelope,
) -> Result<(), GitAiError> {
    let payload = &event.payload;
    let trace_event = payload
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let sid = payload
        .get("sid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if sid.is_empty() {
        return Ok(());
    }
    let root_sid = root_from_sid(sid);

    match trace_event {
        "start" if is_root_sid(sid) => {
            let argv = payload
                .get("argv")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|v| v.as_str().unwrap_or_default().to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let argv_worktree = worktree_from_argv(&argv);
            let mut pending = PendingRootCommand {
                sid: root_sid.clone(),
                start_seq: event.seq,
                start_ns: event.received_at_ns,
                argv,
                name: None,
                worktree: payload
                    .get("repo_working_dir")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                    .or(argv_worktree)
                    .or_else(|| state.sid_worktrees.get(&root_sid).cloned()),
                pre_snapshot: Some(state.last_snapshot.clone()),
                wrapper_mirror: payload
                    .get("wrapper_mirror")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            let (resolved_name, resolved_argv) =
                resolve_command_from_trace_argv(&pending.argv, pending.worktree.as_deref());
            pending.name = resolved_name;
            pending.argv = resolved_argv;
            let should_track_pending = pending
                .name
                .as_deref()
                .map(|name| command_may_mutate_refs(name, &pending.argv))
                .unwrap_or(true);
            if let Some(deferred_exit) = state.deferred_root_exits.remove(&root_sid) {
                if should_track_pending {
                    apply_exit_for_pending_root(
                        runtime,
                        state,
                        pending,
                        deferred_exit.seq,
                        &deferred_exit.payload,
                    )?;
                }
            } else if should_track_pending {
                state.pending_roots.insert(root_sid, pending);
            }
        }
        "def_repo" => {
            if let Some(worktree) = payload.get("worktree").and_then(Value::as_str) {
                state
                    .sid_worktrees
                    .insert(root_sid.clone(), worktree.to_string());
                if let Some(pending) = state.pending_roots.get_mut(&root_sid) {
                    pending.worktree = Some(worktree.to_string());
                    if !pending
                        .name
                        .as_deref()
                        .map(is_tracked_command_name)
                        .unwrap_or(false)
                    {
                        let (resolved_name, resolved_argv) = resolve_command_from_trace_argv(
                            &pending.argv,
                            pending.worktree.as_deref(),
                        );
                        if pending.name.is_none() {
                            pending.name = resolved_name;
                        } else if resolved_name
                            .as_deref()
                            .map(is_tracked_command_name)
                            .unwrap_or(false)
                        {
                            pending.name = resolved_name;
                        }
                        pending.argv = resolved_argv;
                    }
                }
            }
        }
        "cmd_name" if is_root_sid(sid) => {
            if let Some(name) = payload.get("name").and_then(Value::as_str) {
                if is_internal_cmd_name(name) {
                    return Ok(());
                }
                if let Some(pending) = state.pending_roots.get_mut(&root_sid) {
                    let mut drop_pending = false;
                    let existing_name_is_tracked = pending
                        .name
                        .as_deref()
                        .map(is_tracked_command_name)
                        .unwrap_or(false);
                    if !existing_name_is_tracked || is_tracked_command_name(name) {
                        pending.name = Some(name.to_string());
                    }
                    if pending.worktree.is_none() {
                        pending.worktree = worktree_from_argv(&pending.argv)
                            .or_else(|| state.sid_worktrees.get(&root_sid).cloned());
                    }
                    if pending.pre_snapshot.is_none() {
                        pending.pre_snapshot = Some(state.last_snapshot.clone());
                    }
                    let candidate_name = pending
                        .name
                        .as_deref()
                        .or_else(|| argv_primary_command(&pending.argv))
                        .unwrap_or_default()
                        .to_string();
                    if !command_may_mutate_refs(&candidate_name, &pending.argv) {
                        drop_pending = true;
                    }
                    if drop_pending {
                        state.pending_roots.remove(&root_sid);
                        state.deferred_root_exits.remove(&root_sid);
                    }
                } else {
                    let argv = payload
                        .get("argv")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .map(|v| v.as_str().unwrap_or_default().to_string())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let worktree = payload
                        .get("worktree")
                        .and_then(Value::as_str)
                        .or_else(|| payload.get("repo_working_dir").and_then(Value::as_str))
                        .map(ToString::to_string)
                        .or_else(|| worktree_from_argv(&argv))
                        .or_else(|| state.sid_worktrees.get(&root_sid).cloned());
                    let pre_snapshot = Some(state.last_snapshot.clone());
                    if command_may_mutate_refs(name, &argv) {
                        state.pending_roots.insert(
                            root_sid.clone(),
                            PendingRootCommand {
                                sid: root_sid.clone(),
                                start_seq: event.seq,
                                start_ns: event.received_at_ns,
                                argv,
                                name: Some(name.to_string()),
                                worktree,
                                pre_snapshot,
                                wrapper_mirror: payload
                                    .get("wrapper_mirror")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                            },
                        );
                    }
                }
            }
        }
        "exit" if is_root_sid(sid) => {
            if let Some(pending) = state.pending_roots.remove(&root_sid) {
                apply_exit_for_pending_root(runtime, state, pending, event.seq, payload)?;
            } else if trace_payload_may_mutate_refs(payload) {
                state.deferred_root_exits.insert(
                    root_sid,
                    DeferredRootExit {
                        seq: event.seq,
                        received_at_ns: event.received_at_ns,
                        payload: payload.clone(),
                    },
                );
            }
        }
        _ => {}
    }

    Ok(())
}

fn maybe_reanchor_family_state(
    runtime: &FamilyRuntime,
    state: &mut FamilyState,
    cursor: u64,
) -> Result<bool, GitAiError> {
    let latest_seq = runtime.store.latest_seq()?;
    if latest_seq != cursor {
        return Ok(false);
    }

    let force = state.reflog_anchor.is_none() || state.reflog_drifted;
    if !force {
        if !state.pending_roots.is_empty() || !state.deferred_root_exits.is_empty() {
            return Ok(false);
        }
        let now = now_unix_nanos();
        let idle_since = state
            .last_event_applied_ns
            .or(state.last_reanchor_ns)
            .unwrap_or(now);
        if now.saturating_sub(idle_since) < REANCHOR_IDLE_NS {
            return Ok(false);
        }
    }

    ensure_reflog_anchor(runtime, state, cursor, force)
}

fn ensure_reflog_anchor(
    runtime: &FamilyRuntime,
    state: &mut FamilyState,
    cursor: u64,
    force: bool,
) -> Result<bool, GitAiError> {
    if !force && state.reflog_anchor.is_some() {
        return Ok(false);
    }

    let seq_before = runtime.store.latest_seq()?;
    if seq_before != cursor {
        return Ok(false);
    }

    let snapshot = snapshot_common_dir(&runtime.store.common_dir).unwrap_or_default();
    let anchor = capture_reflog_anchor(&runtime.store.common_dir, seq_before)?;
    let seq_after = runtime.store.latest_seq()?;
    if seq_after != seq_before {
        return Ok(false);
    }

    state.last_snapshot = snapshot;
    state.reflog_anchor = Some(anchor);
    state.reflog_drifted = false;
    state.last_reanchor_ns = Some(now_unix_nanos());
    Ok(true)
}

fn capture_reflog_anchor(common_dir: &Path, at_seq: u64) -> Result<ReflogAnchorState, GitAiError> {
    let mut cursors = Vec::new();
    for (path, reference) in discover_reflog_files(common_dir)? {
        let metadata = fs::metadata(&path)?;
        let offset = metadata.len();
        cursors.push(ReflogCursorState {
            path: path.to_string_lossy().to_string(),
            reference,
            offset,
        });
    }
    cursors.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ReflogAnchorState {
        at_seq,
        anchored_at_ns: now_unix_nanos(),
        cursors,
    })
}

fn discover_reflog_files(common_dir: &Path) -> Result<Vec<(PathBuf, String)>, GitAiError> {
    let mut out = Vec::new();
    let logs_dir = common_dir.join("logs");
    if !logs_dir.exists() {
        return Ok(out);
    }
    discover_reflog_files_recursive(&logs_dir, &logs_dir, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn discover_reflog_files_recursive(
    root: &Path,
    current: &Path,
    out: &mut Vec<(PathBuf, String)>,
) -> Result<(), GitAiError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            discover_reflog_files_recursive(root, &path, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let reference = relative.to_string_lossy().replace('\\', "/");
        if reference == "HEAD" || reference.starts_with("refs/") {
            out.push((path, reference));
        }
    }
    Ok(())
}

fn consume_reflog_ref_changes(
    runtime: &FamilyRuntime,
    state: &mut FamilyState,
    seq: u64,
) -> Result<Vec<RefChange>, GitAiError> {
    if state.reflog_anchor.is_none() {
        let _ = ensure_reflog_anchor(runtime, state, seq, true);
    }
    let Some(anchor) = state.reflog_anchor.as_mut() else {
        return Ok(vec![]);
    };

    let discovered = discover_reflog_files(&runtime.store.common_dir)?;
    let mut known_paths: HashSet<String> = anchor.cursors.iter().map(|c| c.path.clone()).collect();
    for (path, reference) in discovered {
        let path_str = path.to_string_lossy().to_string();
        if known_paths.insert(path_str.clone()) {
            anchor.cursors.push(ReflogCursorState {
                path: path_str,
                reference,
                offset: 0,
            });
        }
    }
    anchor.cursors.sort_by(|a, b| a.path.cmp(&b.path));

    let mut raw_changes: Vec<RefChange> = Vec::new();
    for cursor in &mut anchor.cursors {
        let path = PathBuf::from(&cursor.path);
        if !path.exists() {
            continue;
        }
        let metadata = fs::metadata(&path)?;
        let file_len = metadata.len();
        if file_len < cursor.offset {
            state.reflog_drifted = true;
            state.last_error = Some(format!(
                "reflog cursor rewound for {} (offset {} > len {})",
                cursor.path, cursor.offset, file_len
            ));
            return Ok(vec![]);
        }
        if file_len == cursor.offset {
            continue;
        }
        let mut file = OpenOptions::new().read(true).open(&path)?;
        file.seek(SeekFrom::Start(cursor.offset))?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if let Some(change) = parse_reflog_line(&cursor.reference, &line) {
                raw_changes.push(change);
            }
        }
        cursor.offset = file_len;
    }

    if raw_changes.is_empty() {
        return Ok(vec![]);
    }

    anchor.at_seq = seq;
    Ok(raw_changes)
}

fn parse_reflog_line(reference: &str, line: &str) -> Option<RefChange> {
    let head = line.split('\t').next().unwrap_or_default();
    let mut parts = head.split_whitespace();
    let old = parts.next()?.to_string();
    let new = parts.next()?.to_string();
    if !is_valid_oid(&old) || !is_valid_oid(&new) || old == new {
        return None;
    }
    Some(RefChange {
        reference: reference.to_string(),
        old,
        new,
    })
}

fn apply_ref_changes_to_snapshot(pre: &RepoSnapshot, ref_changes: &[RefChange]) -> RepoSnapshot {
    let mut post = pre.clone();
    for change in ref_changes {
        if change.reference == "HEAD" {
            if is_valid_oid(&change.new) && !is_zero_oid(&change.new) {
                post.head = Some(change.new.clone());
            }
            continue;
        }
        if !change.reference.starts_with("refs/") {
            continue;
        }
        if is_valid_oid(&change.new) && !is_zero_oid(&change.new) {
            post.refs.insert(change.reference.clone(), change.new.clone());
        } else {
            post.refs.remove(&change.reference);
        }
        if let Some(branch) = post.branch.as_ref()
            && change.reference == format!("refs/heads/{}", branch)
            && is_valid_oid(&change.new)
            && !is_zero_oid(&change.new)
        {
            post.head = Some(change.new.clone());
        }
    }
    if let Some(branch) = post.branch.as_ref() {
        let branch_ref = format!("refs/heads/{}", branch);
        if let Some(oid) = post.refs.get(&branch_ref)
            && is_valid_oid(oid)
            && !is_zero_oid(oid)
        {
            post.head = Some(oid.clone());
        }
    }
    post
}

fn update_snapshot_head_from_branch(snapshot: &mut RepoSnapshot) {
    let Some(branch) = snapshot.branch.clone() else {
        return;
    };
    let branch_ref = format!("refs/heads/{}", branch);
    if let Some(oid) = snapshot.refs.get(&branch_ref)
        && is_valid_oid(oid)
        && !is_zero_oid(oid)
    {
        snapshot.head = Some(oid.clone());
    }
}

fn infer_branch_after_command(name: &str, argv: &[String], pre_branch: Option<&str>) -> Option<String> {
    match name {
        "switch" => infer_switch_branch(argv).or_else(|| pre_branch.map(ToString::to_string)),
        "checkout" => infer_checkout_branch(argv).or_else(|| pre_branch.map(ToString::to_string)),
        _ => pre_branch.map(ToString::to_string),
    }
}

fn infer_switch_branch(argv: &[String]) -> Option<String> {
    let args = args_after_subcommand(argv, "switch");
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            break;
        }
        if arg == "-c" || arg == "-C" {
            return args.get(i + 1).cloned();
        }
        if arg.starts_with('-') {
            i = i.saturating_add(1);
            continue;
        }
        return Some(args[i].clone());
    }
    None
}

fn infer_checkout_branch(argv: &[String]) -> Option<String> {
    let args = args_after_subcommand(argv, "checkout");
    if args.iter().any(|arg| arg == "--detach") {
        return None;
    }
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            break;
        }
        if arg == "-b" || arg == "-B" {
            return args.get(i + 1).cloned();
        }
        if arg.starts_with('-') {
            i = i.saturating_add(1);
            continue;
        }
        return Some(args[i].clone());
    }
    None
}

fn parse_checkpoint_id(payload: &Value, seq: u64) -> String {
    payload
        .get("checkpoint_id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("checkpoint-{}", seq))
}

fn apply_checkpoint_event(
    runtime: &FamilyRuntime,
    state: &mut FamilyState,
    event: &EventEnvelope,
) -> Result<(), GitAiError> {
    let payload = &event.payload;
    let checkpoint_id = parse_checkpoint_id(payload, event.seq);
    let entries_hash = short_hash_json(payload.get("entries").unwrap_or(&Value::Null));
    let transcript_hash = resolve_transcript_hash(payload)?;
    let dedupe_key = format!(
        "{}:{}:{}",
        checkpoint_id,
        entries_hash,
        transcript_hash.as_deref().unwrap_or("none")
    );
    if state.dedupe_checkpoints.contains(&dedupe_key) {
        return Ok(());
    }
    state.dedupe_checkpoints.insert(dedupe_key);

    let kind = payload
        .get("kind")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    if transcript_hash.is_none()
        && kind
            .as_deref()
            .map(|k| k == "ai_agent" || k == "ai_tab")
            .unwrap_or(false)
    {
        state.unresolved_transcripts.insert(checkpoint_id.clone());
    } else {
        state.unresolved_transcripts.remove(&checkpoint_id);
    }

    let summary = CheckpointSummary {
        kind,
        author: payload
            .get("author")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        agent_id: payload.get("agent_id").cloned(),
        entries_hash,
        transcript_hash,
        line_stats: payload
            .get("line_stats")
            .cloned()
            .unwrap_or_else(|| json!({})),
    };
    state
        .checkpoints
        .insert(checkpoint_id.clone(), summary.clone());
    runtime
        .store
        .append_checkpoint_index(&checkpoint_id, &summary)?;

    if runtime.mode.apply_side_effects() {
        let _ = apply_checkpoint_side_effect(payload);
    }
    Ok(())
}

fn apply_reconcile_event(
    runtime: &FamilyRuntime,
    state: &mut FamilyState,
    event: &EventEnvelope,
) -> Result<(), GitAiError> {
    let before = state.last_snapshot.clone();
    let after = snapshot_common_dir(&runtime.store.common_dir)?;
    let ref_changes = diff_refs(&before.refs, &after.refs);
    state.last_snapshot = after.clone();
    state.last_reconcile_ns = Some(now_unix_nanos());
    let record = json!({
        "seq": event.seq,
        "kind": "reconcile",
        "reason": event.payload.get("reason").cloned().unwrap_or(Value::String("unknown".to_string())),
        "before_head": before.head,
        "after_head": after.head,
        "ref_changes": ref_changes,
    });
    runtime.store.append_reconcile_record(&record)?;
    if !ref_changes.is_empty() {
        state.rewrite_events.push(json!({
            "ref_reconcile": {
                "command": "reconcile",
                "ref_changes": ref_changes
            }
        }));
    }
    Ok(())
}

fn apply_env_override_event(
    state: &mut FamilyState,
    event: &EventEnvelope,
) -> Result<(), GitAiError> {
    let payload = &event.payload;
    let worktree = payload
        .get("repo_working_dir")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| "__family__".to_string());
    let env_map = payload
        .get("env")
        .cloned()
        .ok_or_else(|| GitAiError::Generic("env override payload missing env".to_string()))
        .and_then(|value| {
            serde_json::from_value::<HashMap<String, String>>(value).map_err(GitAiError::from)
        })?;
    state.env_overrides_by_worktree.insert(worktree, env_map);
    Ok(())
}

fn resolve_transcript_hash(payload: &Value) -> Result<Option<String>, GitAiError> {
    if let Some(transcript) = payload.get("transcript")
        && !transcript.is_null()
    {
        return Ok(Some(short_hash_json(transcript)));
    }

    let metadata = payload.get("agent_metadata").and_then(Value::as_object);
    let transcript_path = metadata
        .and_then(|m| m.get("transcript_path").and_then(Value::as_str))
        .or_else(|| metadata.and_then(|m| m.get("chat_session_path").and_then(Value::as_str)));
    let Some(path) = transcript_path else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = format!("{:x}", hasher.finalize());
    Ok(Some(digest))
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
    let request: CheckpointRunPayload = serde_json::from_value(payload.clone()).unwrap_or_default();
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

fn apply_pull_fast_forward_working_log_side_effect(
    worktree: &str,
    old_head: &str,
    new_head: &str,
) -> Result<(), GitAiError> {
    let repo = find_repository_in_path(worktree)?;
    repo.storage.rename_working_log(old_head, new_head)?;
    Ok(())
}

fn apply_pull_fast_forward_working_log_side_effect_from_common_dir(
    common_dir: &Path,
    old_head: &str,
    new_head: &str,
) -> Result<(), GitAiError> {
    let repo = from_bare_repository(common_dir)?;
    repo.storage.rename_working_log(old_head, new_head)?;
    Ok(())
}

fn apply_rewrite_side_effect(
    worktree: &str,
    rewrite_event: RewriteLogEvent,
    env_overrides: Option<&HashMap<String, String>>,
) -> Result<(), GitAiError> {
    let mut repo = find_repository_in_path(worktree)?;
    let author = repo.git_author_identity().name_or_unknown();
    if let RewriteLogEvent::Reset { reset } = &rewrite_event {
        apply_reset_working_log_side_effect(&repo, reset, &author)?;
    }
    if let RewriteLogEvent::Stash { stash } = &rewrite_event {
        apply_stash_rewrite_side_effect(&mut repo, stash)?;
    }
    apply_env_overrides_to_working_log(&repo, &rewrite_event, env_overrides)?;
    repo.handle_rewrite_log_event(rewrite_event, author, true, true);
    Ok(())
}

fn apply_rewrite_side_effect_from_common_dir(
    common_dir: &Path,
    rewrite_event: RewriteLogEvent,
    env_overrides: Option<&HashMap<String, String>>,
) -> Result<(), GitAiError> {
    let mut repo = from_bare_repository(common_dir)?;
    let author = repo.git_author_identity().name_or_unknown();
    apply_env_overrides_to_working_log(&repo, &rewrite_event, env_overrides)?;
    repo.handle_rewrite_log_event(rewrite_event, author, true, true);
    Ok(())
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

fn argv_primary_command(argv: &[String]) -> Option<&str> {
    let args = trace_argv_invocation_tokens(argv);
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if matches!(arg, "-C" | "-c" | "--git-dir" | "--work-tree") {
            i = i.saturating_add(2);
            continue;
        }
        if arg.starts_with('-') {
            i = i.saturating_add(1);
            continue;
        }
        return Some(arg);
    }
    None
}

fn worktree_from_argv(argv: &[String]) -> Option<String> {
    let args = trace_argv_invocation_tokens(argv);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-C" => {
                if i + 1 < args.len() {
                    let candidate = PathBuf::from(&args[i + 1]);
                    let normalized = candidate
                        .canonicalize()
                        .unwrap_or(candidate)
                        .to_string_lossy()
                        .to_string();
                    if !normalized.is_empty() {
                        return Some(normalized);
                    }
                }
                i = i.saturating_add(2);
            }
            "-c" | "--git-dir" | "--work-tree" => {
                i = i.saturating_add(2);
            }
            _ => {
                i = i.saturating_add(1);
            }
        }
    }
    None
}

fn is_tracked_command_name(name: &str) -> bool {
    matches!(
        name,
        "commit"
            | "switch"
            | "checkout"
            | "pull"
            | "push"
            | "stash"
            | "reset"
            | "rebase"
            | "cherry-pick"
            | "merge"
    )
}

fn command_may_mutate_refs(name: &str, argv: &[String]) -> bool {
    if is_tracked_command_name(name) {
        return true;
    }
    if matches!(
        name,
        "update-ref" | "commit-tree" | "branch" | "tag" | "worktree"
    ) {
        return true;
    }
    let primary = argv_primary_command(argv).unwrap_or_default();
    matches!(
        primary,
        "commit"
            | "switch"
            | "checkout"
            | "pull"
            | "push"
            | "stash"
            | "reset"
            | "rebase"
            | "cherry-pick"
            | "merge"
            | "update-ref"
            | "commit-tree"
            | "branch"
            | "tag"
            | "worktree"
    )
}

fn trace_payload_may_mutate_refs(payload: &Value) -> bool {
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let argv = payload
        .get("argv")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    command_may_mutate_refs(name, &argv)
}

fn parse_alias_tokens_for_daemon(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim_start();
    if trimmed.starts_with('!') {
        return None;
    }

    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in trimmed.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                current.push(ch);
            }
            continue;
        }

        if in_double {
            match ch {
                '"' => in_double = false,
                '\\' => escaped = true,
                _ => current.push(ch),
            }
            continue;
        }

        match ch {
            '\'' => in_single = true,
            '"' => in_double = true,
            '\\' => escaped = true,
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        current.push('\\');
    }
    if in_single || in_double {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Some(tokens)
}

fn resolve_alias_invocation_for_daemon(
    parsed: &ParsedGitInvocation,
    repository: &Repository,
) -> Option<ParsedGitInvocation> {
    let mut current = parsed.clone();
    let mut seen: HashSet<String> = HashSet::new();

    loop {
        let command = match current.command.as_deref() {
            Some(command) => command,
            None => return Some(current),
        };

        if !seen.insert(command.to_string()) {
            return None;
        }

        let key = format!("alias.{}", command);
        let alias_value = match repository.config_get_str(&key) {
            Ok(Some(value)) => value,
            _ => return Some(current),
        };

        let alias_tokens = parse_alias_tokens_for_daemon(&alias_value)?;
        let mut expanded_args = Vec::new();
        expanded_args.extend(current.global_args.iter().cloned());
        expanded_args.extend(alias_tokens);
        expanded_args.extend(current.command_args.iter().cloned());
        current = parse_git_cli_args(&expanded_args);
    }
}

fn resolve_alias_invocation_from_trace_argv(
    argv: &[String],
    worktree: Option<&str>,
) -> Option<ParsedGitInvocation> {
    let invocation = parse_git_cli_args(trace_argv_invocation_tokens(argv));
    let Some(command) = invocation.command.as_deref() else {
        return Some(invocation);
    };
    if is_tracked_command_name(command) {
        return Some(invocation);
    }

    let repository = if let Some(worktree) = worktree {
        find_repository_in_path(worktree).ok()
    } else {
        find_repository(&invocation.global_args).ok()
    }?;

    resolve_alias_invocation_for_daemon(&invocation, &repository)
}

fn resolve_command_from_trace_argv(
    argv: &[String],
    worktree: Option<&str>,
) -> (Option<String>, Vec<String>) {
    let fallback_name = argv_primary_command(argv).map(ToString::to_string);
    let fallback_argv = argv.to_vec();
    let Some(resolved) = resolve_alias_invocation_from_trace_argv(argv, worktree) else {
        return (fallback_name, fallback_argv);
    };

    let mut normalized_argv = Vec::new();
    if trace_argv_has_executable_prefix(argv)
        && let Some(first) = argv.first()
    {
        normalized_argv.push(first.clone());
    }
    normalized_argv.extend(resolved.to_invocation_vec());
    if normalized_argv.is_empty() {
        normalized_argv = fallback_argv;
    }

    (
        resolved.command.clone().or_else(|| fallback_name.clone()),
        normalized_argv,
    )
}

fn is_internal_cmd_name(name: &str) -> bool {
    name.starts_with("_run_") || name == "_parse_opt_" || name == "_run_git_alias_"
}

fn is_valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_zero_oid(oid: &str) -> bool {
    is_valid_oid(oid) && oid.chars().all(|c| c == '0')
}

fn branch_ref_name(snapshot: &RepoSnapshot) -> Option<String> {
    snapshot
        .branch
        .as_ref()
        .map(|branch| format!("refs/heads/{}", branch))
}

fn ref_change_bounds_for_reference(
    reference: &str,
    ref_changes: &[RefChange],
) -> Option<(String, String)> {
    let mut first_old: Option<String> = None;
    let mut last_new: Option<String> = None;
    for change in ref_changes.iter().filter(|c| c.reference == reference) {
        if !is_valid_oid(&change.old)
            || !is_valid_oid(&change.new)
            || is_zero_oid(&change.old)
            || is_zero_oid(&change.new)
        {
            continue;
        }
        if first_old.is_none() {
            first_old = Some(change.old.clone());
        }
        last_new = Some(change.new.clone());
    }
    match (first_old, last_new) {
        (Some(old), Some(new)) => Some((old, new)),
        _ => None,
    }
}

fn preferred_branch_ref_change_bounds(
    pre: &RepoSnapshot,
    post: &RepoSnapshot,
    ref_changes: &[RefChange],
) -> Option<(String, String)> {
    if let Some(branch_ref) = branch_ref_name(post)
        && let Some(bounds) = ref_change_bounds_for_reference(&branch_ref, ref_changes)
    {
        return Some(bounds);
    }
    if let Some(branch_ref) = branch_ref_name(pre)
        && let Some(bounds) = ref_change_bounds_for_reference(&branch_ref, ref_changes)
    {
        return Some(bounds);
    }
    let mut first_old: Option<String> = None;
    let mut last_new: Option<String> = None;
    for change in ref_changes.iter().filter(|change| {
        change.reference.starts_with("refs/heads/")
            && is_valid_oid(&change.old)
            && is_valid_oid(&change.new)
            && !is_zero_oid(&change.old)
            && !is_zero_oid(&change.new)
    }) {
        if first_old.is_none() {
            first_old = Some(change.old.clone());
        }
        last_new = Some(change.new.clone());
    }
    match (first_old, last_new) {
        (Some(old), Some(new)) => Some((old, new)),
        _ => None,
    }
}

fn resolve_command_heads(
    pre: &RepoSnapshot,
    post: &RepoSnapshot,
    ref_changes: &[RefChange],
) -> (Option<String>, Option<String>) {
    let mut old_head = pre.head.clone();
    let mut new_head = post.head.clone();
    if let Some((old, new)) = preferred_branch_ref_change_bounds(pre, post, ref_changes) {
        old_head = Some(old);
        new_head = Some(new);
    }
    (old_head, new_head)
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

fn cherry_pick_worktree_key(worktree: Option<&str>) -> String {
    worktree.unwrap_or("__family__").to_string()
}

fn cherry_pick_continue_flag(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "--continue")
}

fn cherry_pick_abort_flag(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "--abort")
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

fn should_synthesize_rewrite_from_snapshots(
    name: &str,
    argv: &[String],
    pre: &RepoSnapshot,
    post: &RepoSnapshot,
) -> bool {
    let head_changed = pre.head.is_some() && post.head.is_some() && pre.head != post.head;
    let rebase_operation = name == "rebase";
    let commit_operation = name == "commit";
    let pull_rebase_operation = name == "pull" && argv.iter().any(|arg| arg == "--rebase");
    let explicit_abort =
        matches!(name, "rebase" | "cherry-pick") && argv.iter().any(|arg| arg == "--abort");
    let explicit_continue =
        matches!(name, "rebase" | "cherry-pick") && argv.iter().any(|arg| arg == "--continue");
    let stash_operation = name == "stash";
    let merge_squash_operation = name == "merge" && argv.iter().any(|arg| arg == "--squash");
    let reset_without_pathspec = name == "reset" && !is_reset_pathspec_command(argv);
    head_changed
        || rebase_operation
        || commit_operation
        || pull_rebase_operation
        || explicit_abort
        || explicit_continue
        || stash_operation
        || merge_squash_operation
        || reset_without_pathspec
}

fn is_reset_pathspec_command(argv: &[String]) -> bool {
    let Some(reset_pos) = argv.iter().position(|arg| arg == "reset") else {
        return false;
    };
    argv.iter().skip(reset_pos + 1).any(|arg| arg == "--")
}

fn synthesize_rewrite_event(
    name: &str,
    argv: &[String],
    pre: &RepoSnapshot,
    post: &RepoSnapshot,
    ref_changes: &[RefChange],
    worktree: Option<&str>,
    active_cherry_pick: Option<&ActiveCherryPickState>,
) -> Option<RewriteLogEvent> {
    match name {
        "commit" => {
            let (old_head, new_head) = resolve_command_heads(pre, post, ref_changes);
            let new_head = new_head.or_else(|| post.head.clone()).unwrap_or_default();
            if new_head.is_empty() {
                return None;
            }
            if argv.iter().any(|a| a == "--amend") {
                let old_head = old_head.or_else(|| pre.head.clone()).unwrap_or_default();
                if old_head.is_empty() {
                    return None;
                }
                Some(RewriteLogEvent::commit_amend(old_head, new_head))
            } else {
                let base_head = old_head.filter(|oid| !is_zero_oid(oid));
                Some(RewriteLogEvent::commit(base_head, new_head))
            }
        }
        "reset" => {
            let kind = if argv.iter().any(|a| a == "--hard") {
                ResetKind::Hard
            } else if argv.iter().any(|a| a == "--soft") {
                ResetKind::Soft
            } else {
                ResetKind::Mixed
            };
            let (old_head, new_head) = resolve_command_heads(pre, post, ref_changes);
            Some(RewriteLogEvent::reset(ResetEvent::new(
                kind,
                argv.iter().any(|a| a == "--keep"),
                argv.iter().any(|a| a == "--merge"),
                new_head.unwrap_or_default(),
                old_head.unwrap_or_default(),
            )))
        }
        "rebase" => {
            if argv.iter().any(|arg| arg == "--abort") {
                Some(RewriteLogEvent::rebase_abort(RebaseAbortEvent::new(
                    pre.head
                        .clone()
                        .or_else(|| post.head.clone())
                        .unwrap_or_default(),
                )))
            } else {
                let (original_head, new_head) = resolve_command_heads(pre, post, ref_changes);
                let original_head = original_head.unwrap_or_default();
                let new_head = new_head.unwrap_or_default();
                let (original_commits, new_commits) =
                    if !original_head.is_empty() && !new_head.is_empty() {
                        build_pull_rebase_mappings_best_effort(worktree, &original_head, &new_head)
                    } else {
                        (vec![], vec![])
                    };
                let original_commits = if original_commits.is_empty() && !original_head.is_empty() {
                    vec![original_head.clone()]
                } else {
                    original_commits
                };
                Some(RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
                    original_head.clone(),
                    new_head.clone(),
                    argv.iter().any(|a| a == "-i" || a == "--interactive"),
                    original_commits,
                    new_commits,
                )))
            }
        }
        "merge" => {
            if !argv.iter().any(|arg| arg == "--squash") {
                return None;
            }
            let source_branch = merge_squash_source_spec_from_argv(argv)?;
            let source_head =
                resolve_commit_specs_to_oids(worktree, std::slice::from_ref(&source_branch))
                    .into_iter()
                    .last()
                    .unwrap_or_default();
            let base_head = pre
                .head
                .clone()
                .or_else(|| post.head.clone())
                .unwrap_or_default();
            if source_head.is_empty() || base_head.is_empty() {
                return None;
            }
            let base_branch = pre
                .branch
                .clone()
                .or_else(|| post.branch.clone())
                .unwrap_or_else(|| "HEAD".to_string());
            Some(RewriteLogEvent::merge_squash(MergeSquashEvent::new(
                source_branch,
                source_head,
                base_branch,
                base_head,
            )))
        }
        "cherry-pick" => {
            if cherry_pick_abort_flag(argv) {
                Some(RewriteLogEvent::cherry_pick_abort(
                    CherryPickAbortEvent::new(
                        pre.head
                            .clone()
                            .or_else(|| post.head.clone())
                            .unwrap_or_default(),
                    ),
                ))
            } else {
                let (old_head, new_head_opt) = resolve_command_heads(pre, post, ref_changes);
                let new_head = new_head_opt.clone().unwrap_or_default();
                let source_specs = cherry_pick_source_specs_from_argv(argv);
                let mut source_commits = if cherry_pick_continue_flag(argv) {
                    active_cherry_pick
                        .map(|state| state.source_commits.clone())
                        .unwrap_or_default()
                } else {
                    resolve_commit_specs_to_oids(worktree, &source_specs)
                };

                let mut original_head = if cherry_pick_continue_flag(argv) {
                    active_cherry_pick
                        .map(|state| state.original_head.clone())
                        .unwrap_or_default()
                } else {
                    old_head.clone().unwrap_or_default()
                };

                if original_head.is_empty() {
                    original_head = old_head.clone().unwrap_or_default();
                }
                if source_commits.is_empty()
                    && let Some(old_head) = old_head.clone()
                    && is_valid_oid(&old_head)
                    && !is_zero_oid(&old_head)
                {
                    source_commits.push(old_head);
                }

                let mut new_commits =
                    cherry_pick_created_commits_best_effort(worktree, &original_head, &new_head);
                if new_commits.is_empty() && !new_head.is_empty() {
                    new_commits.push(new_head.clone());
                }
                let (source_commits, new_commits) =
                    align_cherry_pick_commits(source_commits, new_commits);
                Some(RewriteLogEvent::cherry_pick_complete(
                    CherryPickCompleteEvent::new(
                        original_head,
                        new_head,
                        source_commits,
                        new_commits,
                    ),
                ))
            }
        }
        "stash" => {
            let (operation, stash_ref) = stash_operation_and_ref_from_argv(argv);
            let stash_sha =
                stash_sha_from_pre_snapshot(&operation, stash_ref.as_deref(), pre, worktree);
            Some(RewriteLogEvent::stash(StashEvent::new(
                operation,
                stash_ref,
                stash_sha,
                true,
                vec![],
            )))
        }
        "pull" => {
            let (old_head, new_head) = resolve_command_heads(pre, post, ref_changes);
            if !pull_uses_rebase(argv, worktree) {
                None
            } else {
                let original_head = old_head.unwrap_or_default();
                let new_head = new_head.unwrap_or_default();
                if original_head.is_empty() && new_head.is_empty() {
                    return None;
                }
                let (original_commits, new_commits) =
                    if !original_head.is_empty() && !new_head.is_empty() {
                        build_rebase_mappings_best_effort(worktree, &original_head, &new_head)
                    } else {
                        (vec![], vec![])
                    };
                let original_commits = if original_commits.is_empty() && !original_head.is_empty() {
                    vec![original_head.clone()]
                } else {
                    original_commits
                };
                Some(RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
                    original_head.clone(),
                    new_head.clone(),
                    false,
                    original_commits,
                    new_commits,
                )))
            }
        }
        _ => None,
    }
}

fn stash_operation_and_ref_from_argv(argv: &[String]) -> (StashOperation, Option<String>) {
    let args_after_stash = argv
        .iter()
        .position(|arg| arg == "stash")
        .and_then(|idx| argv.get(idx + 1..))
        .unwrap_or(&[]);
    let subcommand = args_after_stash
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .map(String::as_str);
    let stash_ref = args_after_stash
        .iter()
        .find(|arg| arg.starts_with("stash@{"))
        .cloned();

    let operation = match subcommand {
        Some("apply") => StashOperation::Apply,
        Some("pop") => StashOperation::Pop,
        Some("drop") => StashOperation::Drop,
        Some("list") => StashOperation::List,
        Some("push") | Some("save") | Some("create") | Some("store") => StashOperation::Create,
        Some(_) | None => StashOperation::Create,
    };

    (operation, stash_ref)
}

fn stash_sha_from_pre_snapshot(
    operation: &StashOperation,
    stash_ref: Option<&str>,
    pre: &RepoSnapshot,
    worktree: Option<&str>,
) -> Option<String> {
    if !matches!(operation, StashOperation::Apply | StashOperation::Pop) {
        return None;
    }

    if let Some(sha) = pre.refs.get("refs/stash")
        && is_valid_oid(sha)
        && !is_zero_oid(sha)
    {
        return Some(sha.clone());
    }

    let Some(stash_ref) = stash_ref else {
        return None;
    };
    if is_valid_oid(stash_ref) && !is_zero_oid(stash_ref) {
        return Some(stash_ref.to_string());
    }
    let Some(worktree) = worktree else {
        return None;
    };
    let resolved = run_git_capture(worktree, &["rev-parse", stash_ref]).ok()?;
    if is_valid_oid(&resolved) && !is_zero_oid(&resolved) {
        Some(resolved)
    } else {
        None
    }
}

fn pull_uses_rebase(argv: &[String], worktree: Option<&str>) -> bool {
    if argv
        .iter()
        .any(|arg| arg == "--no-rebase" || arg.starts_with("--no-rebase="))
    {
        return false;
    }
    if argv
        .iter()
        .any(|arg| arg == "--rebase" || arg == "-r" || arg.starts_with("--rebase="))
    {
        return true;
    }
    let Some(worktree) = worktree else {
        return false;
    };
    let Ok(repo) = find_repository_in_path(worktree) else {
        return false;
    };
    let config = repo
        .config_get_regexp(r"^(pull\.rebase)$")
        .unwrap_or_default();
    config
        .get("pull.rebase")
        .map(|value| value.to_ascii_lowercase() != "false")
        .unwrap_or(false)
}

fn merge_squash_source_spec_from_argv(argv: &[String]) -> Option<String> {
    let args = args_after_subcommand(argv, "merge");
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            return args.get(i + 1).cloned();
        }
        if arg.starts_with('-') {
            if merge_option_consumes_value(arg) {
                i = i.saturating_add(2);
            } else {
                i = i.saturating_add(1);
            }
            continue;
        }
        return Some(args[i].clone());
    }
    None
}

fn merge_option_consumes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-m" | "--message"
            | "-F"
            | "--file"
            | "-X"
            | "--strategy-option"
            | "-s"
            | "--strategy"
            | "--into-name"
            | "--cleanup"
            | "-S"
            | "--gpg-sign"
    )
}

fn snapshot_common_dir(common_dir: &Path) -> Result<RepoSnapshot, GitAiError> {
    let head = run_git_capture_common(common_dir, &["rev-parse", "HEAD"]).ok();
    let branch =
        run_git_capture_common(common_dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let refs_raw = run_git_capture_common(
        common_dir,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )
    .unwrap_or_default();
    let mut refs = HashMap::new();
    for line in refs_raw.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 2 {
            refs.insert(parts[0].to_string(), parts[1].to_string());
        }
    }
    Ok(RepoSnapshot { head, branch, refs })
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

fn run_git_capture_common(common_dir: &Path, args: &[&str]) -> Result<String, GitAiError> {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(common_dir)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(GitAiError::Generic(format!(
            "git command failed for common_dir {}: git --git-dir {} {}",
            common_dir.display(),
            common_dir.display(),
            args.join(" ")
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn diff_refs(before: &HashMap<String, String>, after: &HashMap<String, String>) -> Vec<RefChange> {
    let mut refs: BTreeSet<String> = BTreeSet::new();
    refs.extend(before.keys().cloned());
    refs.extend(after.keys().cloned());
    let zero = "0".repeat(40);
    let mut out = Vec::new();
    for reference in refs {
        let old = before
            .get(&reference)
            .cloned()
            .unwrap_or_else(|| zero.clone());
        let new = after
            .get(&reference)
            .cloned()
            .unwrap_or_else(|| zero.clone());
        if old != new {
            out.push(RefChange {
                reference,
                old,
                new,
            });
        }
    }
    out
}

fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<(), GitAiError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let serialized = serde_json::to_string(value)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(serialized.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

fn checksum_for(
    seq: u64,
    repo_family: &str,
    source: &str,
    event_type: &str,
    received_at_ns: u128,
    payload: &Value,
) -> String {
    let canonical = serde_json::to_vec(&json!({
        "seq": seq,
        "repo_family": repo_family,
        "source": source,
        "type": event_type,
        "received_at_ns": received_at_ns,
        "payload": payload
    }))
    .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical);
    format!("{:x}", hasher.finalize())
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

fn control_listener_loop(
    control_socket_path: PathBuf,
    coordinator: Arc<DaemonCoordinator>,
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
            if let Err(e) = handle_control_connection(stream, coord, handle) {
                debug_log(&format!("daemon control connection error: {}", e));
            }
        });
    }
    Ok(())
}

fn handle_control_connection(
    stream: LocalSocketStream,
    coordinator: Arc<DaemonCoordinator>,
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

fn trace_listener_loop(
    trace_socket_path: PathBuf,
    coordinator: Arc<DaemonCoordinator>,
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
            if let Err(e) = handle_trace_connection(stream, coord, handle) {
                debug_log(&format!("daemon trace connection error: {}", e));
            }
        });
    }
    Ok(())
}

fn handle_trace_connection(
    stream: LocalSocketStream,
    coordinator: Arc<DaemonCoordinator>,
    runtime_handle: tokio::runtime::Handle,
) -> Result<(), GitAiError> {
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

pub async fn run_daemon(config: DaemonConfig) -> Result<(), GitAiError> {
    config.ensure_parent_dirs()?;
    let _lock = DaemonLock::acquire(&config.lock_path)?;
    write_pid_metadata(&config)?;
    remove_socket_if_exists(&config.trace_socket_path)?;
    remove_socket_if_exists(&config.control_socket_path)?;

    let coordinator = Arc::new(DaemonCoordinator::new(config.clone()));
    let rt_handle = tokio::runtime::Handle::current();
    let control_socket_path = config.control_socket_path.clone();
    let trace_socket_path = config.trace_socket_path.clone();

    let control_coord = coordinator.clone();
    let control_shutdown_coord = coordinator.clone();
    let control_handle = rt_handle.clone();
    let control_thread = std::thread::spawn(move || {
        if let Err(e) = control_listener_loop(control_socket_path, control_coord, control_handle) {
            debug_log(&format!("daemon control listener exited with error: {}", e));
            // Ensure the daemon exits instead of waiting forever if listener bind/loop fails.
            control_shutdown_coord.request_shutdown();
        }
    });

    let trace_coord = coordinator.clone();
    let trace_shutdown_coord = coordinator.clone();
    let trace_handle = rt_handle.clone();
    let trace_thread = std::thread::spawn(move || {
        if let Err(e) = trace_listener_loop(trace_socket_path, trace_coord, trace_handle) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn init_repo(path: &Path) {
        let output = Command::new("git")
            .arg("init")
            .arg("--initial-branch=main")
            .arg(path)
            .output()
            .expect("git init should run");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_daemon_lock_is_singleton() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("daemon.lock");
        let first = DaemonLock::acquire(&lock_path).expect("first lock should succeed");
        let second = DaemonLock::acquire(&lock_path);
        assert!(second.is_err(), "second lock acquisition should fail");
        drop(first);
        let third = DaemonLock::acquire(&lock_path);
        assert!(third.is_ok(), "lock should be acquirable after drop");
    }

    #[test]
    fn test_family_store_appends_and_reads_events() {
        let dir = tempdir().unwrap();
        let common_dir = dir.path().join(".git");
        fs::create_dir_all(&common_dir).unwrap();
        let store = FamilyStore::for_common_dir(&common_dir).unwrap();
        let first = store
            .append_event(
                common_dir.to_string_lossy().as_ref(),
                "checkpoint",
                CHECKPOINT_EVENT_TYPE,
                json!({"checkpoint_id": "cp1"}),
            )
            .unwrap();
        let second = store
            .append_event(
                common_dir.to_string_lossy().as_ref(),
                "trace2",
                TRACE_EVENT_TYPE,
                json!({"event":"start","sid":"abc","argv":["git","status"]}),
            )
            .unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);
        let events = store.read_events_after(0).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 2);
    }

    #[test]
    fn test_checkpoint_event_marks_unresolved_transcript() {
        let dir = tempdir().unwrap();
        let common_dir = dir.path().join(".git");
        fs::create_dir_all(&common_dir).unwrap();
        let store = FamilyStore::for_common_dir(&common_dir).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let runtime = FamilyRuntime {
            store,
            mode: DaemonMode::Shadow,
            append_lock: AsyncMutex::new(()),
            notify_tx: tx,
            applied_seq: AtomicU64::new(0),
            applied_notify: Notify::new(),
        };
        let mut state = FamilyState::default();
        let event = EventEnvelope {
            seq: 1,
            repo_family: "x".to_string(),
            source: "checkpoint".to_string(),
            event_type: CHECKPOINT_EVENT_TYPE.to_string(),
            received_at_ns: 0,
            payload: json!({
                "checkpoint_id": "cp-1",
                "kind": "ai_agent",
                "author": "dev",
                "agent_metadata": {"transcript_path": "/tmp/not-found-transcript.json"}
            }),
            checksum: "".to_string(),
        };
        apply_checkpoint_event(&runtime, &mut state, &event).unwrap();
        assert!(state.unresolved_transcripts.contains("cp-1"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_family_worker_replays_and_advances_cursor() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let common_dir = repo.join(".git");
        let store = FamilyStore::for_common_dir(&common_dir).unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let runtime = Arc::new(FamilyRuntime {
            store: store.clone(),
            mode: DaemonMode::Shadow,
            append_lock: AsyncMutex::new(()),
            notify_tx: tx,
            applied_seq: AtomicU64::new(0),
            applied_notify: Notify::new(),
        });

        let worker_runtime = runtime.clone();
        let worker = tokio::spawn(async move { family_worker_loop(worker_runtime, rx).await });

        let _ = store
            .append_event(
                common_dir.to_string_lossy().as_ref(),
                "checkpoint",
                CHECKPOINT_EVENT_TYPE,
                json!({"checkpoint_id":"cp-1","kind":"human"}),
            )
            .unwrap();
        let _ = runtime.notify_tx.send(());
        runtime.wait_for_applied(1).await;

        assert_eq!(store.load_cursor().unwrap(), 1);
        assert_eq!(runtime.applied_seq.load(Ordering::SeqCst), 1);

        drop(runtime.notify_tx.clone());
        worker.abort();
    }

    #[test]
    fn test_checkpoint_degraded_then_recovered_transcript() {
        let mut state = FamilyState::default();
        let unresolved = EventEnvelope {
            seq: 1,
            repo_family: "x".to_string(),
            source: "checkpoint".to_string(),
            event_type: CHECKPOINT_EVENT_TYPE.to_string(),
            received_at_ns: 1,
            payload: json!({
                "checkpoint_id": "cp-1",
                "kind": "ai_agent",
                "entries": [{"path":"file.txt"}],
                "agent_metadata": {"transcript_path": "/tmp/does-not-exist.jsonl"}
            }),
            checksum: "x".to_string(),
        };

        let dir = tempdir().unwrap();
        let common_dir = dir.path().join(".git");
        fs::create_dir_all(&common_dir).unwrap();
        let store = FamilyStore::for_common_dir(&common_dir).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let runtime = FamilyRuntime {
            store,
            mode: DaemonMode::Shadow,
            append_lock: AsyncMutex::new(()),
            notify_tx: tx,
            applied_seq: AtomicU64::new(0),
            applied_notify: Notify::new(),
        };

        apply_checkpoint_event(&runtime, &mut state, &unresolved).unwrap();
        assert!(state.unresolved_transcripts.contains("cp-1"));

        let transcript_path = dir.path().join("transcript.jsonl");
        fs::write(&transcript_path, r#"{"role":"user","content":"hello"}"#).unwrap();
        let resolved = EventEnvelope {
            seq: 2,
            repo_family: "x".to_string(),
            source: "checkpoint".to_string(),
            event_type: CHECKPOINT_EVENT_TYPE.to_string(),
            received_at_ns: 2,
            payload: json!({
                "checkpoint_id": "cp-1",
                "kind": "ai_agent",
                "entries": [{"path":"file.txt"}],
                "agent_metadata": {"transcript_path": transcript_path.to_string_lossy().to_string()}
            }),
            checksum: "y".to_string(),
        };
        apply_checkpoint_event(&runtime, &mut state, &resolved).unwrap();
        assert!(!state.unresolved_transcripts.contains("cp-1"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_family_worker_burst_backlog_drains() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let common_dir = repo.join(".git");
        let store = FamilyStore::for_common_dir(&common_dir).unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let runtime = Arc::new(FamilyRuntime {
            store: store.clone(),
            mode: DaemonMode::Shadow,
            append_lock: AsyncMutex::new(()),
            notify_tx: tx,
            applied_seq: AtomicU64::new(0),
            applied_notify: Notify::new(),
        });
        let worker_runtime = runtime.clone();
        let worker = tokio::spawn(async move { family_worker_loop(worker_runtime, rx).await });

        let total = 300;
        for i in 0..total {
            let _ = store
                .append_event(
                    common_dir.to_string_lossy().as_ref(),
                    "checkpoint",
                    CHECKPOINT_EVENT_TYPE,
                    json!({
                        "checkpoint_id": format!("cp-{i}"),
                        "kind": "human"
                    }),
                )
                .unwrap();
        }
        let _ = runtime.notify_tx.send(());
        runtime.wait_for_applied(total as u64).await;
        assert_eq!(store.load_cursor().unwrap(), total as u64);

        worker.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn test_worker_crash_recovery_replays_remaining_events() {
        use tokio::time::{Duration, timeout};

        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let common_dir = repo.join(".git");
        let store = FamilyStore::for_common_dir(&common_dir).unwrap();

        let total = 10_u64;
        for i in 0..total {
            let _ = store
                .append_event(
                    common_dir.to_string_lossy().as_ref(),
                    "checkpoint",
                    CHECKPOINT_EVENT_TYPE,
                    json!({"checkpoint_id": format!("cp-{i}"), "kind":"human"}),
                )
                .unwrap();
        }

        let (tx1, rx1) = mpsc::unbounded_channel();
        let runtime1 = Arc::new(FamilyRuntime {
            store: store.clone(),
            mode: DaemonMode::Shadow,
            append_lock: AsyncMutex::new(()),
            notify_tx: tx1,
            applied_seq: AtomicU64::new(0),
            applied_notify: Notify::new(),
        });
        let worker1_runtime = runtime1.clone();
        let worker1 = tokio::spawn(async move { family_worker_loop(worker1_runtime, rx1).await });
        let _ = runtime1.notify_tx.send(());
        timeout(Duration::from_secs(10), runtime1.wait_for_applied(3))
            .await
            .expect("first worker should apply initial events");
        worker1.abort();

        let (tx2, rx2) = mpsc::unbounded_channel();
        let runtime2 = Arc::new(FamilyRuntime {
            store: store.clone(),
            mode: DaemonMode::Shadow,
            append_lock: AsyncMutex::new(()),
            notify_tx: tx2,
            applied_seq: AtomicU64::new(0),
            applied_notify: Notify::new(),
        });
        let worker2_runtime = runtime2.clone();
        let worker2 = tokio::spawn(async move { family_worker_loop(worker2_runtime, rx2).await });
        let _ = runtime2.notify_tx.send(());
        timeout(Duration::from_secs(10), runtime2.wait_for_applied(total))
            .await
            .expect("restarted worker should replay remaining events");
        assert_eq!(store.load_cursor().unwrap(), total);

        worker2.abort();
    }

    #[test]
    fn test_trace_exit_before_start_is_deferred_then_applied() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let common_dir = repo.join(".git");
        let store = FamilyStore::for_common_dir(&common_dir).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let runtime = FamilyRuntime {
            store,
            mode: DaemonMode::Shadow,
            append_lock: AsyncMutex::new(()),
            notify_tx: tx,
            applied_seq: AtomicU64::new(0),
            applied_notify: Notify::new(),
        };
        let mut state = FamilyState::default();
        let worktree = repo.to_string_lossy().to_string();

        let exit = EventEnvelope {
            seq: 1,
            repo_family: common_dir.to_string_lossy().to_string(),
            source: "trace2".to_string(),
            event_type: TRACE_EVENT_TYPE.to_string(),
            received_at_ns: 1,
            payload: json!({
                "event": "exit",
                "sid": "root-sid",
                "name": "checkout",
                "argv": ["git", "checkout", "main"],
                "code": 0,
                "worktree": worktree,
                "repo_working_dir": worktree
            }),
            checksum: "x".to_string(),
        };
        apply_trace_event(&runtime, &mut state, &exit).unwrap();
        assert_eq!(state.commands.len(), 0);
        assert_eq!(state.deferred_root_exits.len(), 1);

        let start = EventEnvelope {
            seq: 2,
            repo_family: common_dir.to_string_lossy().to_string(),
            source: "trace2".to_string(),
            event_type: TRACE_EVENT_TYPE.to_string(),
            received_at_ns: 2,
            payload: json!({
                "event": "start",
                "sid": "root-sid",
                "argv": ["git", "checkout", "main"],
                "worktree": repo.to_string_lossy().to_string(),
                "repo_working_dir": repo.to_string_lossy().to_string()
            }),
            checksum: "y".to_string(),
        };
        apply_trace_event(&runtime, &mut state, &start).unwrap();

        assert!(
            state.deferred_root_exits.is_empty(),
            "deferred exit should be consumed when start arrives"
        );
        assert!(
            state.pending_roots.is_empty(),
            "pending root should be drained after deferred exit is applied"
        );
        assert_eq!(state.commands.len(), 1);
        let applied = &state.commands[0];
        assert_eq!(applied.sid, "root-sid");
        assert_eq!(applied.name, "checkout");
        assert_eq!(applied.exit_code, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn test_trace_buffer_flush_orders_events_and_applies_command() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let common_dir = repo.join(".git");
        let daemon_dir = dir.path().join("daemon");
        let config = DaemonConfig {
            internal_dir: dir.path().join("internal"),
            lock_path: daemon_dir.join("daemon.lock"),
            trace_socket_path: daemon_dir.join("trace2.sock"),
            control_socket_path: daemon_dir.join("control.sock"),
            mode: DaemonMode::Shadow,
        };
        let coordinator = Arc::new(DaemonCoordinator::new(config));
        let worktree = repo.to_string_lossy().to_string();

        let cmd_resp = coordinator
            .ingest_trace_payload(
                json!({
                    "event": "cmd_name",
                    "sid": "root-ordered",
                    "name": "checkout",
                    "t_abs": 3.0
                }),
                false,
            )
            .await
            .expect("cmd_name ingest should succeed");
        assert_eq!(
            cmd_resp
                .data
                .as_ref()
                .and_then(|v| v.get("buffered"))
                .and_then(Value::as_bool),
            Some(true)
        );

        coordinator
            .ingest_trace_payload(
                json!({
                    "event": "exit",
                    "sid": "root-ordered",
                    "code": 0,
                    "t_abs": 4.0
                }),
                false,
            )
            .await
            .expect("exit ingest should succeed");

        coordinator
            .ingest_trace_payload(
                json!({
                    "event": "start",
                    "sid": "root-ordered",
                    "argv": ["git", "checkout", "main"],
                    "t_abs": 2.0
                }),
                false,
            )
            .await
            .expect("start ingest should succeed");

        let def_repo_resp = coordinator
            .ingest_trace_payload(
                json!({
                    "event": "def_repo",
                    "sid": "root-ordered",
                    "worktree": worktree,
                    "repo_working_dir": repo.to_string_lossy().to_string(),
                    "t_abs": 1.0
                }),
                false,
            )
            .await
            .expect("def_repo ingest should resolve family and flush buffer");

        let applied_seq = def_repo_resp.seq.expect("def_repo should return a sequence");
        let barrier = coordinator
            .wait_through_seq(repo.to_string_lossy().to_string(), applied_seq)
            .await
            .expect("barrier should succeed");
        assert_eq!(barrier.applied_seq, Some(applied_seq));

        let store = FamilyStore::for_common_dir(&common_dir).unwrap();
        let state = store.load_state().expect("family state should be readable");
        assert!(
            state.pending_roots.is_empty() && state.deferred_root_exits.is_empty(),
            "buffer flush should leave no pending/deferred roots"
        );
        assert_eq!(state.commands.len(), 1);

        let command = &state.commands[0];
        assert_eq!(command.sid, "root-ordered");
        assert_eq!(command.name, "checkout");
        assert_eq!(
            command.argv,
            vec!["git".to_string(), "checkout".to_string(), "main".to_string()]
        );
        assert_eq!(
            command.seq, 3,
            "command should be applied at the buffered exit sequence after sorted start/cmd/exit"
        );
        assert_eq!(
            command.worktree.as_deref(),
            Some(repo.to_string_lossy().as_ref()),
            "buffered events should inherit resolved worktree before flush"
        );
    }

    #[test]
    fn test_reconcile_event_updates_last_snapshot() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        init_repo(&repo_path);

        let common_dir = repo_path.join(".git");
        let store = FamilyStore::for_common_dir(&common_dir).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let runtime = FamilyRuntime {
            store,
            mode: DaemonMode::Shadow,
            append_lock: AsyncMutex::new(()),
            notify_tx: tx,
            applied_seq: AtomicU64::new(0),
            applied_notify: Notify::new(),
        };
        let mut state = FamilyState::default();
        let event = EventEnvelope {
            seq: 1,
            repo_family: common_dir.to_string_lossy().to_string(),
            source: "control".to_string(),
            event_type: RECONCILE_EVENT_TYPE.to_string(),
            received_at_ns: 1,
            payload: json!({"reason":"test"}),
            checksum: "unused".to_string(),
        };
        apply_reconcile_event(&runtime, &mut state, &event).unwrap();
        assert!(state.last_reconcile_ns.is_some());
    }
}
