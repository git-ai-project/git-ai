use crate::config;
use crate::error::GitAiError;
use crate::git::rewrite_log::{
    CherryPickAbortEvent, CherryPickCompleteEvent, RebaseAbortEvent, RebaseCompleteEvent,
    ResetEvent, ResetKind, RewriteLogEvent,
};
use crate::git::{find_repository_in_path, from_bare_repository};
use crate::utils::debug_log;
use crate::{
    authorship::working_log::CheckpointKind,
    commands::checkpoint_agent::agent_presets::AgentRunResult,
};
use interprocess::local_socket::{LocalSocketListener, LocalSocketStream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
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
const COMMAND_INDEX_FILE: &str = "command_index.jsonl";
const CHECKPOINT_INDEX_FILE: &str = "checkpoint_index.jsonl";
const PID_META_FILE: &str = "daemon.pid.json";

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
pub struct FamilyState {
    pub pending_roots: HashMap<String, PendingRootCommand>,
    pub commands: Vec<AppliedCommand>,
    pub checkpoints: HashMap<String, CheckpointSummary>,
    pub unresolved_transcripts: BTreeSet<String>,
    pub rewrite_events: Vec<Value>,
    pub last_snapshot: RepoSnapshot,
    pub dedupe_trace: BTreeSet<String>,
    pub dedupe_checkpoints: BTreeSet<String>,
    #[serde(default)]
    pub consumed_reflog: BTreeSet<String>,
    pub last_error: Option<String>,
    pub last_reconcile_ns: Option<u128>,
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
        let mut payload = payload;
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
            let pending = self
                .pending_trace_by_root
                .lock()
                .map_err(|_| GitAiError::Generic("pending trace map lock poisoned".to_string()))?
                .remove(root_sid)
                .unwrap_or_default();
            for mut buffered_payload in pending {
                enrich_trace_payload_with_snapshots(&common_dir, &mut buffered_payload);
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
        enrich_trace_payload_with_snapshots(&common_dir, &mut payload);

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

    pub async fn status_for_family(
        self: &Arc<Self>,
        repo_working_dir: String,
    ) -> Result<FamilyStatus, GitAiError> {
        let (family_key, common_dir) = self.resolve_family_from_worktree(&repo_working_dir)?;
        let store = FamilyStore::for_common_dir(&common_dir)?;
        let latest_seq = store.latest_seq()?;
        let cursor = store.load_cursor()?;
        let state = store.load_state()?;
        let backlog = latest_seq.saturating_sub(cursor);
        Ok(FamilyStatus {
            family_key,
            mode: self.mode,
            latest_seq,
            cursor,
            backlog,
            unresolved_transcripts: state.unresolved_transcripts.iter().cloned().collect(),
            pending_roots: state.pending_roots.len(),
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

    loop {
        let mut progressed = false;
        let events = runtime.store.read_events_after(cursor)?;
        if !events.is_empty() {
            let mut state = runtime.store.load_state()?;
            if state.last_snapshot.refs.is_empty()
                && let Ok(snapshot) = snapshot_common_dir(&runtime.store.common_dir)
            {
                state.last_snapshot = snapshot;
            }
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
                runtime.store.save_state(&state)?;
                runtime.store.save_cursor(cursor)?;
                runtime.applied_seq.store(cursor, Ordering::SeqCst);
                runtime.applied_notify.notify_waiters();
                progressed = true;
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
            let mut pending = PendingRootCommand {
                sid: root_sid.clone(),
                start_seq: event.seq,
                start_ns: event.received_at_ns,
                argv,
                name: None,
                worktree: payload
                    .get("repo_working_dir")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                pre_snapshot: None,
                wrapper_mirror: payload
                    .get("wrapper_mirror")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            pending.pre_snapshot = parse_payload_snapshot(payload, "pre_snapshot").or_else(|| {
                if let Some(worktree) = pending.worktree.as_ref() {
                    snapshot_repo(worktree).ok()
                } else {
                    snapshot_common_dir(&runtime.store.common_dir).ok()
                }
            });
            state.pending_roots.insert(root_sid, pending);
        }
        "def_repo" => {
            if let Some(worktree) = payload.get("worktree").and_then(Value::as_str)
                && let Some(pending) = state.pending_roots.get_mut(&root_sid)
            {
                pending.worktree = Some(worktree.to_string());
                if pending.pre_snapshot.is_none() {
                    pending.pre_snapshot = parse_payload_snapshot(payload, "pre_snapshot")
                        .or_else(|| snapshot_repo(worktree).ok());
                }
            }
        }
        "cmd_name" if is_root_sid(sid) => {
            if let Some(name) = payload.get("name").and_then(Value::as_str) {
                if is_internal_cmd_name(name) {
                    return Ok(());
                }
                if let Some(pending) = state.pending_roots.get_mut(&root_sid) {
                    pending.name = Some(name.to_string());
                    if pending.pre_snapshot.is_none() {
                        pending.pre_snapshot = parse_payload_snapshot(payload, "pre_snapshot");
                    }
                } else {
                    let worktree = payload
                        .get("worktree")
                        .and_then(Value::as_str)
                        .or_else(|| payload.get("repo_working_dir").and_then(Value::as_str))
                        .map(ToString::to_string);
                    let argv = payload
                        .get("argv")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .map(|v| v.as_str().unwrap_or_default().to_string())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let pre_snapshot =
                        parse_payload_snapshot(payload, "pre_snapshot").or_else(|| {
                            if let Some(worktree) = worktree.as_deref() {
                                snapshot_repo(worktree).ok()
                            } else {
                                snapshot_common_dir(&runtime.store.common_dir).ok()
                            }
                        });
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
        "exit" if is_root_sid(sid) => {
            if let Some(pending) = state.pending_roots.remove(&root_sid) {
                let exit_code = payload.get("code").and_then(Value::as_i64).unwrap_or(1) as i32;
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

                let post_snapshot = pending
                    .worktree
                    .as_ref()
                    .and_then(|worktree| snapshot_repo(worktree).ok())
                    .or_else(|| parse_payload_snapshot(payload, "post_snapshot"))
                    .or_else(|| snapshot_common_dir(&runtime.store.common_dir).ok())
                    .unwrap_or_default();
                let pre_snapshot = pending
                    .pre_snapshot
                    .clone()
                    .or_else(|| parse_payload_snapshot(payload, "pre_snapshot"))
                    .or_else(|| snapshot_common_dir(&runtime.store.common_dir).ok())
                    .unwrap_or_default();
                let ref_changes = diff_refs(&pre_snapshot.refs, &post_snapshot.refs);
                state.last_snapshot = post_snapshot.clone();

                let command = AppliedCommand {
                    seq: event.seq,
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

                if exit_code == 0 {
                    let mut rewrite_event = infer_rewrite_event_from_reflog(
                        &pending,
                        &runtime.store.common_dir,
                        &mut state.consumed_reflog,
                    );
                    if rewrite_event.is_none()
                        && should_synthesize_rewrite_from_snapshots(
                            pending.name.as_deref().unwrap_or_default(),
                            &pending.argv,
                            &pre_snapshot,
                            &post_snapshot,
                        )
                    {
                        rewrite_event = synthesize_rewrite_event(
                            pending.name.as_deref().unwrap_or_default(),
                            &pending.argv,
                            &pre_snapshot,
                            &post_snapshot,
                            &ref_changes,
                        );
                    }
                    if let Some(rewrite_event) = rewrite_event {
                        state
                            .rewrite_events
                            .push(serde_json::to_value(&rewrite_event)?);
                        if state.rewrite_events.len() > 1000 {
                            state
                                .rewrite_events
                                .drain(0..state.rewrite_events.len() - 1000);
                        }
                        if runtime.mode.apply_side_effects() && !pending.wrapper_mirror {
                            if let Some(worktree) = pending.worktree.as_deref() {
                                let _ = apply_rewrite_side_effect(worktree, rewrite_event);
                            } else {
                                let _ = apply_rewrite_side_effect_from_common_dir(
                                    &runtime.store.common_dir,
                                    rewrite_event,
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
                }
            }
        }
        _ => {}
    }

    Ok(())
}

fn parse_payload_snapshot(payload: &Value, key: &str) -> Option<RepoSnapshot> {
    payload
        .get(key)
        .cloned()
        .and_then(|value| serde_json::from_value::<RepoSnapshot>(value).ok())
}

fn snapshot_for_trace_payload(common_dir: &Path, payload: &Value) -> Option<RepoSnapshot> {
    payload
        .get("worktree")
        .and_then(Value::as_str)
        .or_else(|| payload.get("repo_working_dir").and_then(Value::as_str))
        .and_then(|worktree| snapshot_repo(worktree).ok())
        .or_else(|| snapshot_common_dir(common_dir).ok())
}

fn set_payload_snapshot(payload: &mut Value, key: &str, snapshot: RepoSnapshot) {
    if let Some(object) = payload.as_object_mut()
        && let Ok(serialized) = serde_json::to_value(snapshot)
    {
        object.insert(key.to_string(), serialized);
    }
}

fn enrich_trace_payload_with_snapshots(common_dir: &Path, payload: &mut Value) {
    let trace_event = payload
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let sid = payload
        .get("sid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if sid.is_empty() || !is_root_sid(sid) {
        return;
    }

    match trace_event {
        "start" | "cmd_name" => {
            if payload.get("pre_snapshot").is_none()
                && let Some(snapshot) = snapshot_for_trace_payload(common_dir, payload)
            {
                set_payload_snapshot(payload, "pre_snapshot", snapshot);
            }
        }
        "exit" => {
            if payload.get("post_snapshot").is_none()
                && let Some(snapshot) = snapshot_for_trace_payload(common_dir, payload)
            {
                set_payload_snapshot(payload, "post_snapshot", snapshot);
            }
        }
        _ => {}
    }
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

fn apply_rewrite_side_effect(
    worktree: &str,
    rewrite_event: RewriteLogEvent,
) -> Result<(), GitAiError> {
    let mut repo = find_repository_in_path(worktree)?;
    let author = repo.git_author_identity().name_or_unknown();
    repo.handle_rewrite_log_event(rewrite_event, author, true, true);
    Ok(())
}

fn apply_rewrite_side_effect_from_common_dir(
    common_dir: &Path,
    rewrite_event: RewriteLogEvent,
) -> Result<(), GitAiError> {
    let mut repo = from_bare_repository(common_dir)?;
    let author = repo.git_author_identity().name_or_unknown();
    repo.handle_rewrite_log_event(rewrite_event, author, true, true);
    Ok(())
}

fn is_internal_cmd_name(name: &str) -> bool {
    name.starts_with("_run_") || name == "_parse_opt_" || name == "_run_git_alias_"
}

#[derive(Debug, Clone)]
struct HeadReflogEntry {
    old: String,
    new: String,
    message: String,
}

fn infer_rewrite_event_from_reflog(
    pending: &PendingRootCommand,
    common_dir: &Path,
    consumed_reflog: &mut BTreeSet<String>,
) -> Option<RewriteLogEvent> {
    if pending.name.as_deref() != Some("commit") {
        return None;
    }

    let want_amend = pending.argv.iter().any(|arg| arg == "--amend");
    let (entry, dedupe_key) = find_unconsumed_commit_reflog_entry(
        pending.worktree.as_deref(),
        common_dir,
        want_amend,
        consumed_reflog,
    )?;
    if !is_valid_oid(&entry.new)
        || (want_amend && (!is_valid_oid(&entry.old) || entry.old == entry.new))
    {
        return None;
    }

    consumed_reflog.insert(dedupe_key);

    if want_amend {
        return Some(RewriteLogEvent::commit_amend(entry.old, entry.new));
    }
    let base = if is_zero_oid(&entry.old) {
        None
    } else {
        Some(entry.old)
    };
    Some(RewriteLogEvent::commit(base, entry.new))
}

fn find_unconsumed_commit_reflog_entry(
    worktree: Option<&str>,
    common_dir: &Path,
    want_amend: bool,
    consumed_reflog: &BTreeSet<String>,
) -> Option<(HeadReflogEntry, String)> {
    let entries = read_head_reflog_entries(worktree, common_dir)?;
    for entry in entries.into_iter().rev() {
        let is_amend = entry.message.starts_with("commit (amend)");
        let is_commit = entry.message.starts_with("commit");
        if want_amend && !is_amend {
            continue;
        }
        if !want_amend && (!is_commit || is_amend) {
            continue;
        }
        let kind = if want_amend { "amend" } else { "commit" };
        let key = format!("{kind}:{}:{}", entry.old, entry.new);
        if consumed_reflog.contains(&key) {
            continue;
        }
        return Some((entry, key));
    }
    None
}

fn read_head_reflog_entries(
    worktree: Option<&str>,
    common_dir: &Path,
) -> Option<Vec<HeadReflogEntry>> {
    let path = head_reflog_path(worktree, common_dir)?;
    let raw = fs::read_to_string(path).ok()?;
    let mut entries = Vec::new();
    for line in raw.lines() {
        if let Some(entry) = parse_head_reflog_line(line) {
            entries.push(entry);
        }
    }
    Some(entries)
}

fn head_reflog_path(worktree: Option<&str>, common_dir: &Path) -> Option<PathBuf> {
    if let Some(worktree) = worktree {
        let git_dir_raw = run_git_capture(worktree, &["rev-parse", "--git-dir"]).ok()?;
        let git_dir = PathBuf::from(&git_dir_raw);
        let resolved = if git_dir.is_absolute() {
            git_dir
        } else {
            PathBuf::from(worktree).join(git_dir)
        };
        return Some(resolved.join("logs").join("HEAD"));
    }
    Some(common_dir.join("logs").join("HEAD"))
}

fn parse_head_reflog_line(line: &str) -> Option<HeadReflogEntry> {
    let mut parts = line.splitn(3, ' ');
    let old = parts.next()?.to_string();
    let new = parts.next()?.to_string();
    if !is_valid_oid(&old) || !is_valid_oid(&new) {
        return None;
    }
    let message = line
        .split_once('\t')
        .map(|(_, msg)| msg.trim().to_string())
        .unwrap_or_default();
    Some(HeadReflogEntry { old, new, message })
}

fn is_valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_zero_oid(oid: &str) -> bool {
    is_valid_oid(oid) && oid.chars().all(|c| c == '0')
}

fn should_synthesize_rewrite_from_snapshots(
    name: &str,
    argv: &[String],
    pre: &RepoSnapshot,
    post: &RepoSnapshot,
) -> bool {
    let head_changed = pre.head.is_some() && post.head.is_some() && pre.head != post.head;
    let explicit_abort =
        matches!(name, "rebase" | "cherry-pick") && argv.iter().any(|arg| arg == "--abort");
    head_changed || explicit_abort
}

fn synthesize_rewrite_event(
    name: &str,
    argv: &[String],
    pre: &RepoSnapshot,
    post: &RepoSnapshot,
    _ref_changes: &[RefChange],
) -> Option<RewriteLogEvent> {
    match name {
        "commit" => {
            if argv.iter().any(|a| a == "--amend") {
                Some(RewriteLogEvent::commit_amend(
                    pre.head.clone().unwrap_or_default(),
                    post.head.clone().unwrap_or_default(),
                ))
            } else {
                Some(RewriteLogEvent::commit(
                    pre.head.clone(),
                    post.head.clone().unwrap_or_default(),
                ))
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
            Some(RewriteLogEvent::reset(ResetEvent::new(
                kind,
                argv.iter().any(|a| a == "--keep"),
                argv.iter().any(|a| a == "--merge"),
                post.head.clone().unwrap_or_default(),
                pre.head.clone().unwrap_or_default(),
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
                Some(RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
                    pre.head.clone().unwrap_or_default(),
                    post.head.clone().unwrap_or_default(),
                    argv.iter().any(|a| a == "-i" || a == "--interactive"),
                    vec![pre.head.clone().unwrap_or_default()],
                    vec![post.head.clone().unwrap_or_default()],
                )))
            }
        }
        "cherry-pick" => {
            if argv.iter().any(|arg| arg == "--abort") {
                Some(RewriteLogEvent::cherry_pick_abort(
                    CherryPickAbortEvent::new(
                        pre.head
                            .clone()
                            .or_else(|| post.head.clone())
                            .unwrap_or_default(),
                    ),
                ))
            } else {
                Some(RewriteLogEvent::cherry_pick_complete(
                    CherryPickCompleteEvent::new(
                        pre.head.clone().unwrap_or_default(),
                        post.head.clone().unwrap_or_default(),
                        vec![],
                        vec![post.head.clone().unwrap_or_default()],
                    ),
                ))
            }
        }
        _ => None,
    }
}

fn snapshot_repo(worktree: &str) -> Result<RepoSnapshot, GitAiError> {
    let head = run_git_capture(worktree, &["rev-parse", "HEAD"]).ok();
    let branch = run_git_capture(worktree, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let refs_raw = run_git_capture(
        worktree,
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
    async fn test_worker_crash_recovery_replays_remaining_events() {
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
        runtime1.wait_for_applied(3).await;
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
        runtime2.wait_for_applied(total).await;
        assert_eq!(store.load_cursor().unwrap(), total);

        worker2.abort();
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
