# Sweep-Based Transcript Discovery System

**Date:** 2026-04-30  
**Status:** Approved  
**Related PR:** #1217 (sessions-v2 branch)

## Overview

Refactor the transcript discovery system to replace file polling with agent-specific sweep functions. This addresses several architectural issues:

1. **Redundant events:** Eliminate the separate `CheckpointRecorded` control API event by extracting transcript info from the existing `CheckpointRequest`
2. **Inefficient polling:** Replace 1-second file stat polling with 30-minute sweep cycles
3. **Duplicate logic:** Unify transcript reading under a single `Agent` trait, removing format-specific readers scattered across multiple modules
4. **Legacy migration:** Remove `migrate_internal_db()` and old discovery logic, replaced by agent sweep functions

## Architecture

### Discovery Paths

The system has **two** discovery paths that feed sessions into the `TranscriptWorker`:

#### 1. Checkpoint Notifications (Immediate Priority)

When `git-ai checkpoint` fires:
- Sends `CheckpointRequest` to daemon (already happens today)
- Daemon extracts `TranscriptSource` metadata from the request
- If session doesn't exist in `transcripts.db`, creates it immediately
- Queues session for immediate processing (Priority::Immediate)
- No separate `CheckpointRecorded` control API event needed

#### 2. Agent Sweeps (Low Priority, 30-minute intervals)

Every 30 minutes, `SweepCoordinator` runs:
- Calls each agent's `discover_sessions()` sweep function
- Each agent scans its storage (filesystem, DB, etc.) and returns ALL discovered sessions
- Coordinator compares discoveries against `transcripts.db`:
  - **New sessions:** Insert into DB, queue for processing
  - **Behind sessions:** File has grown or changed since last watermark, queue for processing
  - **Current sessions:** Watermark matches file state, skip
- All sweep-discovered sessions queued at Priority::Low

### Processing Queue

```
Priority::Immediate (0)  ← Checkpoint notifications
Priority::Low (2)        ← Sweep discoveries

(Priority::High removed - was polling)
```

The `TranscriptWorker` processes the queue with 100ms tick interval, always taking the highest priority task first.

### Data Storage

**In-Memory (ephemeral, lost on daemon restart):**
- `BinaryHeap<ProcessingTask>` - priority queue of sessions waiting to be processed
- `in_flight: HashSet<PathBuf>` - sessions currently being processed

**Persisted in SQLite (`transcripts.db`):**
- Session records (session_id, transcript_path, format, agent_type)
- Watermarks (where we left off reading each transcript)
- File metadata (last_known_size, last_modified timestamp)
- Error tracking (processing_errors, last_error)

On daemon restart, the in-memory queue is empty. The next sweep (within 30 minutes) re-discovers sessions and re-queues anything that's behind.

## Agent Trait Design

### Unified Agent Trait

```rust
// src/transcripts/agent.rs

pub trait Agent: Send + Sync {
    /// Returns the sweep strategy for this agent
    fn sweep_strategy(&self) -> SweepStrategy;
    
    /// Discover all sessions in the agent's storage.
    /// Returns ALL sessions found, regardless of whether they're in transcripts.db.
    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError>;
    
    /// Read transcript incrementally from the given watermark
    fn read_incremental(
        &self,
        path: &Path,
        watermark: Box<dyn WatermarkStrategy>,
        session_id: &str,
    ) -> Result<TranscriptBatch, TranscriptError>;
}

pub enum SweepStrategy {
    /// Periodic polling at the given interval
    Periodic(Duration),
    /// File system watcher (not implemented yet)
    FsWatcher,
    /// HTTP API polling (not implemented yet)
    HttpApi,
    /// No sweep support for this agent
    None,
}

pub struct DiscoveredSession {
    pub session_id: String,
    pub agent_type: String,
    pub transcript_path: PathBuf,
    pub transcript_format: TranscriptFormat,
    pub watermark_type: WatermarkType,
    pub initial_watermark: Box<dyn WatermarkStrategy>,
    pub model: Option<String>,
    pub tool: Option<String>,
    pub external_thread_id: Option<String>,
}
```

### Agent Registry

```rust
pub fn get_agent(agent_type: &str) -> Option<Box<dyn Agent>> {
    match agent_type {
        "claude" => Some(Box::new(ClaudeAgent)),
        "cursor" => Some(Box::new(CursorAgent)),
        "droid" => Some(Box::new(DroidAgent)),
        "copilot" => Some(Box::new(CopilotAgent)),
        // human, mock_ai, etc. don't have sweep support
        _ => None,
    }
}
```

### Example Implementation

```rust
// src/transcripts/agents/claude.rs

pub struct ClaudeAgent;

impl Agent for ClaudeAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::Periodic(Duration::from_secs(30 * 60)) // 30 minutes
    }
    
    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        let mut sessions = Vec::new();
        
        // Scan ~/.config/Claude/User/globalStorage/.../conversations/
        let claude_dir = dirs::config_dir()
            .ok_or(...)?
            .join("Claude/User/globalStorage/.../conversations");
        
        for entry in fs::read_dir(claude_dir)? {
            let path = entry?.path();
            if path.extension() == Some("jsonl") {
                let session_id = extract_session_id_from_path(&path);
                sessions.push(DiscoveredSession {
                    session_id,
                    agent_type: "claude".to_string(),
                    transcript_path: path,
                    transcript_format: TranscriptFormat::ClaudeJsonl,
                    watermark_type: WatermarkType::ByteOffset,
                    initial_watermark: Box::new(ByteOffsetWatermark::new(0)),
                    model: None, // Will be extracted on first read if needed
                    tool: Some("claude".to_string()),
                    external_thread_id: None,
                });
            }
        }
        
        Ok(sessions)
    }
    
    fn read_incremental(
        &self,
        path: &Path,
        watermark: Box<dyn WatermarkStrategy>,
        session_id: &str,
    ) -> Result<TranscriptBatch, TranscriptError> {
        // Move logic from src/transcripts/formats/claude.rs here
        // Parse JSONL, extract events, return batch
    }
}
```

## SweepCoordinator

Orchestrates the 30-minute sweep cycle across all agents.

```rust
// src/daemon/sweep_coordinator.rs

pub struct SweepCoordinator {
    transcripts_db: Arc<TranscriptsDatabase>,
    agent_registry: Vec<(String, Box<dyn Agent>)>,
}

impl SweepCoordinator {
    pub fn new(transcripts_db: Arc<TranscriptsDatabase>) -> Self {
        let agent_registry = vec![
            ("claude".to_string(), Box::new(ClaudeAgent) as Box<dyn Agent>),
            ("cursor".to_string(), Box::new(CursorAgent) as Box<dyn Agent>),
            ("droid".to_string(), Box::new(DroidAgent) as Box<dyn Agent>),
            ("copilot".to_string(), Box::new(CopilotAgent) as Box<dyn Agent>),
        ];
        
        Self { transcripts_db, agent_registry }
    }
    
    /// Run a full sweep across all agents.
    /// Returns sessions that need processing (new or behind).
    pub fn run_sweep(&self) -> Result<Vec<SessionToProcess>, TranscriptError> {
        let mut sessions_to_process = Vec::new();
        
        for (agent_type, agent) in &self.agent_registry {
            // Skip agents that don't support periodic sweeps
            if !matches!(agent.sweep_strategy(), SweepStrategy::Periodic(_)) {
                continue;
            }
            
            // Discover all sessions for this agent
            let discovered = agent.discover_sessions()?;
            
            for session in discovered {
                match self.transcripts_db.get_session(&session.session_id)? {
                    None => {
                        // New session - insert and queue
                        self.insert_new_session(&session)?;
                        sessions_to_process.push(SessionToProcess {
                            session_id: session.session_id.clone(),
                            agent_type: session.agent_type.clone(),
                            canonical_path: canonicalize_path(&session.transcript_path),
                        });
                    }
                    Some(existing) => {
                        // Session exists - check if behind
                        if self.is_session_behind(&session, &existing)? {
                            sessions_to_process.push(SessionToProcess {
                                session_id: session.session_id.clone(),
                                agent_type: session.agent_type.clone(),
                                canonical_path: canonicalize_path(&session.transcript_path),
                            });
                        }
                    }
                }
            }
        }
        
        Ok(sessions_to_process)
    }
    
    fn is_session_behind(&self, discovered: &DiscoveredSession, existing: &SessionRecord) -> Result<bool, TranscriptError> {
        let metadata = fs::metadata(&discovered.transcript_path)?;
        let file_size = metadata.len() as i64;
        let modified = get_modified_timestamp(&metadata);
        
        Ok(file_size != existing.last_known_size 
            || (modified.is_some() && modified != existing.last_modified))
    }
    
    fn insert_new_session(&self, session: &DiscoveredSession) -> Result<(), TranscriptError> {
        let now = Utc::now().timestamp();
        let record = SessionRecord {
            session_id: session.session_id.clone(),
            agent_type: session.agent_type.clone(),
            transcript_path: session.transcript_path.display().to_string(),
            transcript_format: format!("{:?}", session.transcript_format),
            watermark_type: format!("{:?}", session.watermark_type),
            watermark_value: session.initial_watermark.serialize(),
            model: session.model.clone(),
            tool: session.tool.clone(),
            external_thread_id: session.external_thread_id.clone(),
            first_seen_at: now,
            last_processed_at: 0,
            last_known_size: 0,
            last_modified: None,
            processing_errors: 0,
            last_error: None,
        };
        
        self.transcripts_db.insert_session(&record)?;
        Ok(())
    }
}

pub struct SessionToProcess {
    pub session_id: String,
    pub agent_type: String,
    pub canonical_path: PathBuf,
}
```

**Key responsibilities:**
1. Run sweeps across all registered agents
2. Compare discovered sessions against `transcripts.db`
3. Identify new/behind sessions
4. Insert new sessions into DB
5. Return list of sessions to queue for processing

## Refactored TranscriptWorker

**Key changes:**
- Remove polling ticker and `detect_transcript_modifications()`
- Remove `migrate_internal_db()` and old `discover_sessions()`
- Add 30-minute sweep ticker
- Extract checkpoint notification from `CheckpointRequest`
- Simplify processing to use `agent.read_incremental()`

```rust
// src/daemon/transcript_worker.rs

struct TranscriptWorker {
    transcripts_db: Arc<TranscriptsDatabase>,
    sweep_coordinator: SweepCoordinator,
    priority_queue: BinaryHeap<ProcessingTask>,
    in_flight: HashSet<PathBuf>,
    telemetry_handle: DaemonTelemetryWorkerHandle,
    shutdown_notify: Arc<Notify>,
    checkpoint_rx: tokio::sync::mpsc::UnboundedReceiver<CheckpointNotification>,
}

impl TranscriptWorker {
    async fn run(mut self) {
        tracing::info!("transcript worker started");

        let mut processing_ticker = interval(Duration::from_millis(100));
        let mut sweep_ticker = interval(Duration::from_secs(30 * 60));
        
        processing_ticker.tick().await;
        sweep_ticker.tick().await;

        // Run initial sweep on startup
        if let Err(e) = self.run_sweep().await {
            tracing::error!(error = %e, "initial sweep failed");
        }

        loop {
            tokio::select! {
                _ = self.shutdown_notify.notified() => {
                    tracing::info!("transcript worker received shutdown signal");
                    self.drain_immediate_tasks().await;
                    break;
                }
                _ = processing_ticker.tick() => {
                    self.process_next_task().await;
                }
                _ = sweep_ticker.tick() => {
                    if let Err(e) = self.run_sweep().await {
                        tracing::error!(error = %e, "sweep failed");
                    }
                }
                Some(notification) = self.checkpoint_rx.recv() => {
                    self.handle_checkpoint_notification(notification).await;
                }
            }
        }

        tracing::info!("transcript worker shutdown complete");
    }

    async fn run_sweep(&mut self) -> Result<(), String> {
        let sessions = self.sweep_coordinator.run_sweep()
            .map_err(|e| e.to_string())?;
        
        tracing::info!(discovered = sessions.len(), "sweep completed");
        
        for session in sessions {
            if self.in_flight.contains(&session.canonical_path) {
                continue;
            }
            
            self.priority_queue.push(ProcessingTask {
                priority: Priority::Low,
                session_id: session.session_id,
                agent_type: session.agent_type,
                canonical_path: session.canonical_path,
                retry_count: 0,
            });
        }
        
        Ok(())
    }

    async fn handle_checkpoint_notification(&mut self, notification: CheckpointNotification) {
        let canonical_path = std::fs::canonicalize(&notification.transcript_path)
            .unwrap_or_else(|_| notification.transcript_path.clone());

        if self.in_flight.contains(&canonical_path) {
            return;
        }

        self.priority_queue.push(ProcessingTask {
            priority: Priority::Immediate,
            session_id: notification.session_id.clone(),
            agent_type: notification.agent_type,
            canonical_path,
            retry_count: 0,
        });
    }

    fn process_session_blocking(
        db: &TranscriptsDatabase,
        task: &ProcessingTask,
    ) -> Result<(), TranscriptError> {
        let session = db.get_session(&task.session_id)?
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!("session not found: {}", task.session_id),
            })?;

        // Get the agent implementation
        let agent = crate::transcripts::agent::get_agent(&task.agent_type)
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!("unknown agent type: {}", task.agent_type),
            })?;

        // Parse watermark
        let watermark_type = WatermarkType::from_str(&session.watermark_type)?;
        let watermark = watermark_type.deserialize(&session.watermark_value)?;

        // Read transcript using agent
        let batch = agent.read_incremental(
            &PathBuf::from(&session.transcript_path),
            watermark,
            &session.session_id,
        )?;

        let event_count = batch.events.len();

        // Emit events via metrics::record
        for event_values in batch.events {
            let attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
                .session_id(session.session_id.clone());
            record(event_values, attrs);
        }

        // Update watermark and metadata
        db.update_watermark(&session.session_id, batch.new_watermark.as_ref())?;
        
        if let Ok(metadata) = std::fs::metadata(&session.transcript_path) {
            let file_size = metadata.len();
            let modified = get_modified_timestamp(&metadata);
            db.update_file_metadata(&session.session_id, file_size, modified)?;
        }

        tracing::debug!(
            session_id = %task.session_id,
            events = event_count,
            "processed session"
        );

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessingTask {
    priority: Priority,
    session_id: String,
    agent_type: String, // NEW: needed to get the right Agent impl
    canonical_path: PathBuf,
    retry_count: u32,
}

#[derive(Debug, Clone)]
struct CheckpointNotification {
    session_id: String,
    agent_type: String, // NEW: extracted from CheckpointRequest
    trace_id: String,
    transcript_path: PathBuf,
}
```

## Checkpoint Notification Extraction

**Current flow:** `git-ai checkpoint` → sends `CheckpointRequest` to daemon → sends separate `CheckpointRecorded` event

**New flow:** `git-ai checkpoint` → sends `CheckpointRequest` to daemon → daemon extracts transcript info directly

### Changes in Checkpoint Command

```rust
// src/commands/checkpoint.rs

// REMOVE this function entirely:
// fn send_checkpoint_notification(...) { ... }

// Keep the existing checkpoint flow - it already sends CheckpointRequest to daemon
// Just remove the call to send_checkpoint_notification()
```

### Changes in Daemon Control API

```rust
// src/daemon/control_api.rs

pub enum ControlRequest {
    // ... other variants ...
    
    // REMOVE this variant:
    // CheckpointRecorded { session_id, trace_id, transcript_path },
    
    // Keep existing CheckpointRun - it already contains TranscriptSource
}
```

### Changes in Daemon Handler

```rust
// src/daemon.rs (or wherever CheckpointRun is handled)

async fn handle_checkpoint_run(
    request: CheckpointRunRequest,
    transcript_worker_handle: &TranscriptWorkerHandle,
    transcripts_db: &TranscriptsDatabase,
) -> Result<ControlResponse, String> {
    // Run the checkpoint (existing logic)
    let result = run_checkpoint(request)?;
    
    // NEW: Extract transcript info from the request and notify worker
    if let Some(checkpoint_request) = request.checkpoint_request() {
        if let Some(transcript_source) = &checkpoint_request.transcript_source {
            let session_id = transcript_source.session_id.clone();
            let agent_type = extract_agent_type(&checkpoint_request);
            
            // If this is a new session, ensure it exists in transcripts.db
            ensure_session_exists(
                transcripts_db,
                &session_id,
                &agent_type,
                transcript_source,
            ).await?;
            
            // Notify worker for immediate processing
            transcript_worker_handle.notify_checkpoint(
                session_id,
                agent_type,
                checkpoint_request.trace_id(),
                transcript_source.path.clone(),
            ).await;
        }
    }
    
    Ok(result)
}

fn ensure_session_exists(
    db: &TranscriptsDatabase,
    session_id: &str,
    agent_type: &str,
    transcript_source: &TranscriptSource,
) -> Result<(), String> {
    // Check if session exists
    if db.get_session(session_id)?.is_some() {
        return Ok(());
    }
    
    // Create new session record
    let now = Utc::now().timestamp();
    let agent = get_agent(agent_type)
        .ok_or_else(|| format!("unknown agent type: {}", agent_type))?;
    
    let watermark_type = transcript_source.format.default_watermark_type();
    let initial_watermark = watermark_type.create_initial();
    
    let record = SessionRecord {
        session_id: session_id.to_string(),
        agent_type: agent_type.to_string(),
        transcript_path: transcript_source.path.display().to_string(),
        transcript_format: format!("{:?}", transcript_source.format),
        watermark_type: format!("{:?}", watermark_type),
        watermark_value: initial_watermark.serialize(),
        model: transcript_source.model.clone(),
        tool: transcript_source.tool.clone(),
        external_thread_id: transcript_source.external_thread_id.clone(),
        first_seen_at: now,
        last_processed_at: 0,
        last_known_size: 0,
        last_modified: None,
        processing_errors: 0,
        last_error: None,
    };
    
    db.insert_session(&record)?;
    Ok(())
}
```

**Key insight:** The `CheckpointRequest` already contains all the transcript metadata we need via `TranscriptSource`. We extract it and notify the worker directly - no separate control API event needed.

## Metrics Event Changes

### Change 1: Remove session_id from Committed Events

The `committed` event should NOT have `session_id` in its `EventAttributes` because a single commit can contain code from multiple AI sessions.

```rust
// src/commands/hooks/commit_hooks.rs (or wherever committed event is recorded)

// BEFORE (current code - INCORRECT):
let attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
    .session_id(some_session_id)  // ❌ DON'T set this for committed events
    .repo_url(repo_url)
    .branch(branch);

record(CommittedValues::new()..., attrs);

// AFTER (new code - CORRECT):
let attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
    // ✅ No session_id for committed events
    .repo_url(repo_url)
    .branch(branch);

record(CommittedValues::new()..., attrs);
```

**Where session_id/trace_id SHOULD be set:**
- ✅ Checkpoint events (has session context)
- ✅ AgentTrace events (extracted from transcript)
- ✅ AgentUsage events (has session context)

**Where session_id/trace_id should NOT be set:**
- ❌ Committed events (no single session - could be multiple AI sessions in one commit)
- ❌ InstallHooks events (no session context)

### Change 2: Rename tool_use_id to external_tool_use_id

Rename the field in `CheckpointValues` and `AgentTraceValues` to clarify it's an external ID from the agent, not an internal git-ai ID.

```rust
// src/metrics/events.rs

pub mod checkpoint_pos {
    pub const CHECKPOINT_TS: usize = 0;
    pub const KIND: usize = 1;
    pub const FILE_PATH: usize = 2;
    pub const LINES_ADDED: usize = 3;
    pub const LINES_DELETED: usize = 4;
    pub const LINES_ADDED_SLOC: usize = 5;
    pub const LINES_DELETED_SLOC: usize = 6;
    pub const EXTERNAL_TOOL_USE_ID: usize = 7; // RENAMED from TOOL_USE_ID
}

#[derive(Debug, Clone, Default)]
pub struct CheckpointValues {
    pub checkpoint_ts: PosField<u64>,
    pub kind: PosField<String>,
    pub file_path: PosField<String>,
    pub lines_added: PosField<u32>,
    pub lines_deleted: PosField<u32>,
    pub lines_added_sloc: PosField<u32>,
    pub lines_deleted_sloc: PosField<u32>,
    pub external_tool_use_id: PosField<String>, // RENAMED from tool_use_id
}

impl CheckpointValues {
    // Rename builder methods
    pub fn external_tool_use_id(mut self, value: impl Into<String>) -> Self {
        self.external_tool_use_id = Some(Some(value.into()));
        self
    }

    pub fn external_tool_use_id_null(mut self) -> Self {
        self.external_tool_use_id = Some(None);
        self
    }
}

// Similar changes for AgentTraceValues
pub mod agent_trace_pos {
    pub const EVENT_TYPE: usize = 0;
    pub const EVENT_TS: usize = 1;
    pub const EXTERNAL_TOOL_USE_ID: usize = 2; // RENAMED from TOOL_USE_ID
    pub const TOOL_NAME: usize = 3;
    pub const PROMPT_TEXT: usize = 4;
    pub const RESPONSE_TEXT: usize = 5;
}

#[derive(Debug, Clone, Default)]
pub struct AgentTraceValues {
    pub event_type: PosField<String>,
    pub event_ts: PosField<u64>,
    pub external_tool_use_id: PosField<String>, // RENAMED
    pub tool_name: PosField<String>,
    pub prompt_text: PosField<String>,
    pub response_text: PosField<String>,
}
```

**Note:** The position numbers stay the same (7 and 2), so this is backward compatible with existing data.

## Model Extraction Helper

Lightweight helper to extract model name from transcript tail when not provided in hook input.

```rust
// src/transcripts/model_extraction.rs

use super::types::TranscriptError;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

/// Extract model name from the last message in a transcript.
/// Reads from the end of the file backwards to avoid loading the entire transcript.
/// 
/// Returns None if model cannot be determined.
pub fn extract_model_from_tail(
    path: &Path,
    format: TranscriptFormat,
) -> Result<Option<String>, TranscriptError> {
    match format {
        TranscriptFormat::ClaudeJsonl => extract_model_from_jsonl_tail(path, "model"),
        TranscriptFormat::CursorJsonl => extract_model_from_jsonl_tail(path, "model"),
        TranscriptFormat::DroidJsonl => extract_model_from_jsonl_tail(path, "model"),
        TranscriptFormat::CopilotEventStreamJsonl => extract_model_from_jsonl_tail(path, "model"),
        TranscriptFormat::CopilotSessionJson => extract_model_from_session_json(path),
    }
}

fn extract_model_from_jsonl_tail(
    path: &Path,
    model_field: &str,
) -> Result<Option<String>, TranscriptError> {
    let mut file = File::open(path).map_err(|e| TranscriptError::Fatal {
        message: format!("failed to open transcript: {}", e),
    })?;
    
    let file_size = file.metadata().map_err(|e| TranscriptError::Fatal {
        message: format!("failed to get file metadata: {}", e),
    })?.len();
    
    if file_size == 0 {
        return Ok(None);
    }
    
    // Read last 4KB (should be enough for most messages)
    let read_size = std::cmp::min(4096, file_size);
    let seek_pos = file_size - read_size;
    
    file.seek(SeekFrom::Start(seek_pos)).map_err(|e| TranscriptError::Transient {
        message: format!("failed to seek: {}", e),
    })?;
    
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines()
        .filter_map(|l| l.ok())
        .collect();
    
    // Parse last complete line
    if let Some(last_line) = lines.last() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(last_line) {
            if let Some(model) = json.get(model_field).and_then(|v| v.as_str()) {
                return Ok(Some(model.to_string()));
            }
        }
    }
    
    Ok(None)
}

fn extract_model_from_session_json(path: &Path) -> Result<Option<String>, TranscriptError> {
    // For session.json formats, model might be in metadata at top of file
    // Implementation depends on Copilot session.json structure
    Ok(None)
}
```

### Usage in Checkpoint Code

```rust
// src/commands/checkpoint.rs

fn build_checkpoint_attrs(
    repo: &Repository,
    base_commit: &str,
    agent_id: Option<&AgentId>,
    transcript_source: Option<&TranscriptSource>,
) -> EventAttributes {
    let mut attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
        .base_commit_sha(base_commit);
    
    if let Some(agent_id) = agent_id {
        let session_id = generate_session_id(&agent_id.id, &agent_id.tool);
        attrs = attrs
            .session_id(session_id)
            .tool(&agent_id.tool)
            .external_prompt_id(&agent_id.id);
        
        // Try to use model from agent_id first
        if !agent_id.model.is_empty() {
            attrs = attrs.model(&agent_id.model);
        } else if let Some(ts) = transcript_source {
            // Fallback: extract from transcript tail
            if let Ok(Some(model)) = extract_model_from_tail(&ts.path, ts.format) {
                attrs = attrs.model(model);
            }
        }
    }
    
    // Add repo metadata...
    attrs
}
```

**Key points:**
- Only called when model is NOT in hook input
- Reads last 4KB of file (efficient, no full parse)
- Format-aware (different logic per transcript format)
- Returns `None` if can't determine model (graceful degradation)

## File & Module Structure

### New Files to Create

```
src/transcripts/
├── mod.rs                      (updated exports)
├── agent.rs                    (NEW - unified Agent trait + registry)
├── model_extraction.rs         (NEW - tail-reading helper)
├── agents/                     (NEW directory)
│   ├── mod.rs                 (agent registry)
│   ├── claude.rs              (ClaudeAgent - sweep + read)
│   ├── cursor.rs              (CursorAgent - sweep + read)
│   ├── droid.rs               (DroidAgent - sweep + read)
│   ├── copilot.rs             (CopilotAgent - sweep + read)
│   └── ... (more agents)
├── sweep.rs                    (SweepStrategy enum, DiscoveredSession struct)
├── types.rs                    (existing - TranscriptBatch, TranscriptError, etc.)
├── watermark.rs                (existing)
└── db.rs                       (existing - transcripts.db access)

src/daemon/
├── transcript_worker.rs        (MODIFIED - remove polling, add sweep)
├── sweep_coordinator.rs        (NEW - orchestrates sweeps)
└── ... (other daemon files)
```

### Files to DELETE

```
src/transcripts/
├── formats/                    (DELETE entire directory)
│   ├── mod.rs                 ❌
│   ├── claude.rs              ❌
│   ├── cursor.rs              ❌
│   ├── droid.rs               ❌
│   └── copilot.rs             ❌
└── processor.rs                (DELETE - format dispatch no longer needed)

src/commands/checkpoint_agent/
└── transcript_readers.rs       (DELETE - 112KB file)

src/daemon/
└── control_api.rs              (MODIFIED - remove CheckpointRecorded variant)
```

## Migration & Rollout Strategy

### Phase 1: Foundation (no behavior change)
1. Create new file structure (`src/transcripts/agents/`, `sweep_coordinator.rs`)
2. Define `Agent` trait and `SweepStrategy` enum
3. Implement `ClaudeAgent` (sweep + read) as proof-of-concept
4. Keep old code running - no daemon changes yet

### Phase 2: Parallel Implementation
1. Implement remaining agents (Cursor, Droid, Copilot, etc.)
2. Migrate format-specific read logic from `formats/*.rs` to `agents/*.rs`
3. Create `SweepCoordinator` and wire it up
4. Add model extraction helper

### Phase 3: Worker Refactor
1. Refactor `TranscriptWorker` to use new sweep system
2. Remove polling ticker and `detect_transcript_modifications()`
3. Remove `migrate_internal_db()` and old `discover_sessions()`
4. Extract checkpoint notification from `CheckpointRequest`
5. Remove `CheckpointRecorded` control API variant

### Phase 4: Cleanup
1. Delete `src/transcripts/formats/` directory
2. Delete `src/transcripts/processor.rs`
3. Delete `src/commands/checkpoint_agent/transcript_readers.rs`
4. Remove `.session_id()` call from committed event recording
5. Rename `tool_use_id` → `external_tool_use_id` in metrics

### Phase 5: Testing & Verification
1. Verify sweeps discover all sessions correctly
2. Verify checkpoint notifications trigger immediate processing
3. Verify no duplicate processing (in_flight deduplication works)
4. Test daemon restart recovery (queue is empty, next sweep catches up)
5. Verify model extraction helper works for agents without model in hook

## Backward Compatibility

- ✅ Watermark positions stay the same (position 7, 2) - data compatible
- ✅ `transcripts.db` schema unchanged
- ✅ Existing sessions continue working (just processed via sweeps now)
- ✅ No breaking changes to agent presets (they keep working as-is)

## Testing Strategy

1. **Unit tests for each `Agent` implementation** (sweep + read)
2. **Integration test:** spawn daemon, trigger checkpoint, verify immediate processing
3. **Integration test:** add new transcript file, wait for sweep, verify discovery
4. **Integration test:** modify existing transcript, verify next sweep queues it
5. **Test model extraction helper** with various transcript formats
6. **Load test:** 1000+ sessions discovered in one sweep, verify performance

## Summary of Changes

**Remove:**
- Polling (1s ticker and `detect_transcript_modifications()`)
- `migrate_internal_db()` function
- `discover_sessions()` function (old version)
- `CheckpointRecorded` control API event
- `src/commands/checkpoint_agent/transcript_readers.rs` (112KB)
- `src/transcripts/formats/` directory
- `src/transcripts/processor.rs`
- `.session_id()` call from committed event recording

**Add:**
- `Agent` trait (unifies sweep + read)
- `SweepCoordinator` (orchestrates 30-min sweep cycle)
- `src/transcripts/agents/` directory with agent implementations
- `model_extraction.rs` helper for tail-reading transcripts
- 30-minute sweep ticker in `TranscriptWorker`
- Session creation in checkpoint handler

**Simplify:**
- Two discovery paths (checkpoint + sweep) instead of three
- Unified agent interface replaces format dispatch
- Cleaner worker loop (no polling logic)
- Direct checkpoint notification extraction (no separate event)

## Future Extensions

**File System Watcher:**
```rust
impl Agent for ClaudeAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::FsWatcher // Just change this line
    }
}
```

The `TranscriptWorker` would then use `notify` crate to watch directories instead of periodic polling.

**HTTP API Polling:**
```rust
impl Agent for OpenCodeAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::HttpApi // For cloud-based agents
    }
}
```

The worker would poll HTTP endpoints for session updates instead of filesystem.

**Per-Agent Intervals:**
```rust
pub enum SweepStrategy {
    Periodic(Duration), // Per-agent customizable duration
    // ...
}
```

Each agent can specify its own sweep interval if needed.
