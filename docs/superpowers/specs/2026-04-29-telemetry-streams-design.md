# Telemetry Streams Re-implementation Design

**Date**: 2026-04-29  
**Status**: Approved  
**Base**: PR 1198 (sessions-v2-remove-messages)  
**Approach**: Clean-slate rewrite (Approach B)

## Overview

Re-implement the telemetry-streams branch work on top of PR 1198 with architectural improvements:
- New `src/transcripts/` module for all transcript reading logic
- Dedicated transcripts.db SQLite database (replaces internal_db)
- Unified watermarking abstraction supporting multiple strategies
- Long-lived daemon worker for asynchronous transcript processing
- Incremental processing of historical transcripts
- Enhanced telemetry with session_id/trace_id/tool_use_id

## Architecture

### System Structure

Three major components:

1. **`src/transcripts/` module** - Contains transcript reading/processing logic
2. **Transcripts database** - SQLite at `~/.git-ai/transcripts.db` replacing internal_db
3. **Transcript worker** - Long-lived tokio task inside daemon process

### Data Flow

```
Checkpoint → Daemon Worker → Transcript Processor → Transcripts DB (watermark update)
                ↓                    ↓
           Priority Queue      AgentTrace Events → Metrics DB → Upload
```

**Flow steps**:

1. Agent calls `git-ai checkpoint <agent>` with tool_use_id in hook input
2. Checkpoint handler extracts session_id, trace_id, tool_use_id from preset
3. Checkpoint event emitted with all IDs in shared EventAttributes
4. Daemon worker notified of new checkpoint (high priority)
5. Worker reads transcript from watermark position
6. Extracts AgentTrace events, updates watermark
7. Events batched and uploaded to telemetry service

**Background processing**:
- On daemon startup, worker scans known transcript directories
- Discovers any transcripts without watermarks (historical data)
- Queues them at low priority for incremental processing
- Processes during idle time without impacting live checkpoints

### Component Boundaries

- **Transcripts module** (`src/transcripts/`): Reading/parsing logic, database, watermarking
- **Daemon worker** (`src/daemon/transcript_worker.rs`): Orchestration, priority queue, polling
- **Checkpoint command**: Minimal - extracts metadata, delegates to preset
- **Agent presets**: Extract tool_use_id and session context from hook input

## Transcripts Module

### Module Organization

```
src/transcripts/
├── mod.rs              - Public API, re-exports
├── db.rs               - TranscriptsDatabase (SQLite wrapper)
├── watermark.rs        - WatermarkStrategy trait + implementations
├── processor.rs        - TranscriptProcessor (orchestrates reading)
├── formats/            - Format-specific readers
│   ├── mod.rs
│   ├── claude.rs       - Claude Code JSONL
│   ├── cursor.rs       - Cursor JSONL
│   ├── droid.rs        - Droid JSONL
│   ├── copilot.rs      - Copilot session/event-stream
│   └── ...             - Other agent formats
└── types.rs            - Common types (AgentFormat, SessionInfo, etc.)
```

### Key Types

#### WatermarkStrategy Trait

```rust
pub trait WatermarkStrategy: Send + Sync {
    fn serialize(&self) -> String;
    fn advance(&mut self, bytes_read: usize, records_read: usize);
}

// Type-specific deserialization - each implementation provides its own
pub enum WatermarkType {
    ByteOffset,
    RecordIndex,
    Timestamp,
    Hybrid,
}

impl WatermarkType {
    pub fn deserialize(&self, s: &str) -> Result<Box<dyn WatermarkStrategy>, TranscriptError> {
        match self {
            WatermarkType::ByteOffset => Ok(Box::new(ByteOffsetWatermark::from_str(s)?)),
            WatermarkType::RecordIndex => Ok(Box::new(RecordIndexWatermark::from_str(s)?)),
            WatermarkType::Timestamp => Ok(Box::new(TimestampWatermark::from_str(s)?)),
            WatermarkType::Hybrid => Ok(Box::new(HybridWatermark::from_str(s)?)),
        }
    }
}

// Implementations:
pub struct ByteOffsetWatermark(u64);
pub struct RecordIndexWatermark(u64);
pub struct TimestampWatermark(DateTime<Utc>);
pub struct HybridWatermark { 
    offset: u64, 
    record: u64, 
    timestamp: Option<DateTime<Utc>> 
};
```

**Why hybrid watermarking**: Different agent transcript formats require different tracking strategies. Byte offsets work for append-only JSONL files but fail for databases (SQLite) or formats with in-place updates. Record indices work for sequential formats but don't survive log rotation. Timestamps work for time-ordered streams but miss concurrent events. The hybrid approach allows per-agent configuration of the optimal strategy.

**How to apply**: Each agent preset specifies its watermark type. Claude Code uses ByteOffset (JSONL append-only). Droid uses Hybrid (SQLite with timestamp ordering). Cursor uses ByteOffset. The abstraction ensures new agents can plug in without changing core processing logic.

#### TranscriptReader Trait

```rust
pub trait TranscriptReader {
    fn read_incremental(
        &self,
        path: &Path,
        watermark: &dyn WatermarkStrategy,
        session_id: &str,
    ) -> Result<TranscriptBatch, TranscriptError>;
}

pub struct TranscriptBatch {
    pub events: Vec<AgentTraceValues>,
    pub model: Option<String>,
    pub new_watermark: Box<dyn WatermarkStrategy>,
}
```

### TranscriptsDatabase Schema

```sql
CREATE TABLE sessions (
    session_id TEXT PRIMARY KEY,
    agent_type TEXT NOT NULL,
    transcript_path TEXT NOT NULL,
    transcript_format TEXT NOT NULL,
    watermark_type TEXT NOT NULL,
    watermark_value TEXT NOT NULL,
    model TEXT,
    tool TEXT,
    external_thread_id TEXT,
    first_seen_at INTEGER NOT NULL,
    last_processed_at INTEGER NOT NULL,
    last_known_size INTEGER NOT NULL DEFAULT 0,
    last_modified INTEGER,
    processing_errors INTEGER DEFAULT 0,
    last_error TEXT
);

CREATE INDEX idx_sessions_agent_type ON sessions(agent_type);
CREATE INDEX idx_sessions_last_processed ON sessions(last_processed_at);
CREATE INDEX idx_sessions_errors ON sessions(processing_errors) WHERE processing_errors > 0;
CREATE INDEX idx_sessions_transcript_path ON sessions(transcript_path);

CREATE TABLE processing_stats (
    session_id TEXT PRIMARY KEY,
    total_events INTEGER DEFAULT 0,
    total_bytes INTEGER DEFAULT 0,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);
```

**Why separate stats table**: Keeps hot session metadata small for fast queries. Stats updated frequently during processing but rarely read (debugging/monitoring only). Separation allows sessions table to remain compact for worker's priority queue lookups.

**Why transcript_path index**: Checkpoint notifications provide path but not session_id. Worker must look up `session_by_path(path) -> Option<SessionInfo>` for deduplication. Index on transcript_path makes this O(log n) instead of full table scan.

**Why last_known_size and last_modified fields**: Modification detection compares current file metadata against last known state. Size changes indicate new content (most common). mtime changes catch modifications without size change (rare but possible). Both stored as integers for fast comparison.

**How to apply**: 
- Worker queries sessions table for watermarks, paths, and file metadata
- Every 1 second, poll all sessions and compare file size/mtime against database
- After processing, update last_known_size and last_modified
- Checkpoint notifications include transcript_path for deduplication key
- Stats table updated after each batch
- Dashboard queries stats for aggregate metrics (events processed per agent, etc.)

### Migration from internal_db

**Why migration needed**: Existing installations have prompt records in internal_db that reference sessions. Without migration, historical sessions would be invisible to the worker and never processed.

**How to apply**:
1. On daemon startup, check if `~/.git-ai/internal.db` exists
2. If yes, read all prompts records
3. For each prompt with external_thread_id mapping to a known transcript path:
   - Create session record in transcripts.db
   - Set watermark to beginning (will reprocess)
4. Rename internal.db to internal.db.deprecated
5. Log migration summary

Migration runs once per installation. Failed migrations logged but don't block daemon startup (degrades to "no historical data" state).

## Metrics Schema Changes

### EventAttributes Updates

**Add**:
- Position 24: `session_id` (String, required for all events)
- Position 25: `trace_id` (String, nullable - present for agent-related events)

**Remove**:
- Position 22: `prompt_id` field (tombstoned - never reuse this index)

**Why tombstone prompt_id**: Position-encoded schema requires stable indices for backward compatibility. Removing prompt_id but reusing index 22 would break existing server-side parsers. Tombstoning (marking reserved but unused) ensures forward compatibility.

**How to apply**: 
- All event emission sites updated to populate session_id (required)
- Agent-related events (Checkpoint, AgentUsage, AgentTrace) populate trace_id
- Server-side parsers updated to expect new fields at 24/25
- Position 22 remains unused, documented as deprecated

### CheckpointValues Updates

**Add**:
- Position 7: `tool_use_id` (String, nullable)

**Usage**: When agent preset extracts tool_use_id from hook input, it's stored in checkpoint event. The tool_use_id is also propagated to the checkpoint event's EventAttributes.trace_id field. Server-side can join checkpoint events to AgentTrace events via `(session_id, trace_id)` where checkpoint.trace_id matches the agent_trace.trace_id of the tool use that triggered the edit.

**Why session/trace/tool_use_id linking**: Enables server-side analysis of AI agent behavior. Given a checkpoint (file edit), trace back to the exact tool call in the transcript that triggered it. Answers questions like "which Claude tool calls resulted in accepted code?" or "what's the acceptance rate per tool type?"

**How to apply**: Checkpoint command extracts tool_use_id from preset metadata. Stored in both CheckpointValues.tool_use_id (position 7) and EventAttributes.trace_id (position 25). The trace_id in EventAttributes provides the join key. Server joins checkpoint events to agent_trace events where checkpoint.attrs.trace_id == agent_trace.values.trace_id (both contain the tool_use_id).

### Backward Compatibility

**Wire format**: Existing metrics events in flight or queued continue to work:
- Missing session_id/trace_id treated as nullable (graceful degradation server-side)
- Server ignores unknown positions (forward compatible)
- Metrics API version stays at 1 (additive changes don't require bump)

**Database**: Existing metrics.db events can be queried:
- Add migration to backfill session_id from commit context where possible
- Null values acceptable for historical data

## Daemon Worker Architecture

### Worker Structure

**Component**: `TranscriptWorker` - long-lived tokio task spawned during daemon initialization

**State**:
```rust
struct TranscriptWorker {
    transcripts_db: Arc<Mutex<TranscriptsDatabase>>,
    priority_queue: Arc<Mutex<PriorityQueue<ProcessingTask>>>,
    in_flight: HashSet<String>,  // Canonical paths currently being processed
    telemetry_handle: DaemonTelemetryWorkerHandle,
    shutdown_signal: Arc<AtomicBool>,
}

struct ProcessingTask {
    key: String,        // Canonical file path (deduplication key)
    session_id: String,
    priority: Priority,
    retry_count: u32,
}

enum Priority {
    Immediate,    // Checkpoint-triggered (tool_use_id linking)
    High,         // Modification-detected (trailing messages)
    Low,          // Historical backfill
}
```

### Worker Lifecycle

**Startup**:
1. Migrate internal_db if exists
2. Collect transcript directories from all AgentPresets (via transcript_dirs() method)
3. Scan directories for existing transcript files matching known formats
4. Cross-reference with transcripts.db sessions table
5. Queue any untracked sessions at Low priority
6. Initialize last_known_size and last_modified for all sessions from current file metadata
7. Start processing loop (polling + checkpoint notifications)

**Processing Loop**:
```rust
loop {
    select! {
        _ = shutdown_signal.wait() => break,
        _ = interval(100ms).tick() => {
            // Process one task from queue
            if let Some(task) = queue.pop_highest_priority() {
                process_session(task).await;
            }
        }
        _ = interval(1s).tick() => {
            // Detect modifications by checking file metadata
            detect_transcript_modifications().await;
        }
        Some(checkpoint_event) = checkpoint_rx.recv() => {
            let key = canonical_path(&checkpoint_event.transcript_path);
            if !in_flight.contains(&key) {
                queue.push(ProcessingTask {
                    key: key.clone(),
                    session_id: checkpoint_event.session_id,
                    priority: Immediate,
                    retry_count: 0,
                });
                in_flight.insert(key);
            }
        }
    }
}

async fn detect_transcript_modifications(&mut self) {
    // Query all sessions from database
    let sessions = self.db.all_sessions();
    
    for session in sessions {
        let path = Path::new(&session.transcript_path);
        
        // Get current file metadata (size, mtime)
        if let Ok(metadata) = tokio::fs::metadata(path).await {
            let file_size = metadata.len();
            let modified = metadata.modified().ok();
            
            // Compare with last known state in database
            if file_size > session.last_known_size 
                || modified.map(|m| m > session.last_modified).unwrap_or(false) 
            {
                let key = canonical_path(path);
                if !self.in_flight.contains(&key) {
                    self.queue.push(ProcessingTask {
                        key: key.clone(),
                        session_id: session.session_id.clone(),
                        priority: High,
                        retry_count: 0,
                    });
                    self.in_flight.insert(key);
                }
            }
        }
    }
}

// After processing completes
fn on_process_complete(&mut self, key: &str, session_id: &str) {
    self.in_flight.remove(key);
    
    // Update last known size/mtime in database
    if let Ok(metadata) = std::fs::metadata(&session.transcript_path) {
        self.db.update_file_metadata(
            session_id,
            metadata.len(),
            metadata.modified().ok(),
        );
    }
}
```

**Why polling over platform file watchers**: Simple, predictable, and cross-platform. Platform file watchers (inotify, FSEvents, ReadDirectoryChangesW) have different behaviors, edge cases, and resource limits. For our use case (checking ~100s of files every second), polling is negligible overhead and eliminates platform-specific complexity.

**Why 1-second poll interval**: Strikes balance between responsiveness and overhead. Checking 1000 sessions = 1000 stat() calls/sec = ~10ms I/O on SSD. Trailing messages appear within 1 second, which is acceptable latency for telemetry.

**Why deduplication**: A single edit can trigger both a checkpoint notification (from the hook) and a modification detection (from polling). Without deduplication, we'd process the same transcript twice. The canonical file path serves as a unique key, and the in_flight set prevents concurrent/duplicate processing.

**How to apply**: 
- Worker maintains sorted queue (Immediate > High > Low) keyed by canonical transcript path
- Every 1 second, poll all session transcript files for size/mtime changes
- Checkpoint notifications → Immediate priority (process within 100ms for tool_use_id linking)
- Modification detection → High priority (process within 1 second to catch trailing messages)
- Historical sessions discovered at startup → Low priority (backfill incrementally)
- in_flight set tracks actively processing transcripts to prevent duplication
- After processing, update last_known_size and last_modified in database
- Errors move to back with exponential backoff

**Processing Strategy**:
- **Immediate priority**: Checkpoint-triggered, process within 100ms for tool_use_id linking
- **High priority**: Modification-detected, process within 1 second to catch trailing messages
- **Low priority**: Historical backfill, process 1 per 5 seconds when queue has capacity
- **Error handling**: Failed session moves to back of queue with exponential backoff
- **Deduplication**: Canonical file path as key, in_flight set prevents duplicate work

### Integration Points

**Checkpoint command**:
- After recording checkpoint, sends notification to daemon via control socket
- Message: `{"type": "checkpoint_recorded", "session_id": "...", "trace_id": "...", "transcript_path": "..."}`
- Worker receives notification and queues task at Immediate priority
- Ensures tool_use_id linking works (tool call processed before watermark advances)

**Modification detection**:
- Every 1 second, poll all sessions and check file size/mtime against database
- Detects file modifications (writes, appends)
- Catches trailing messages (assistant responses after tool calls that don't trigger hooks)
- Worker queues modified transcripts at High priority

**Why both mechanisms needed**: Checkpoints only fire on tool executions. If an assistant writes a follow-up message after the last tool call, no checkpoint fires and that message would be missed. Polling catches these trailing messages within 1 second.

**Example scenario**:
1. T=0s: Tool call (Edit) → checkpoint fires → Immediate processing (catches tool call, updates size/mtime)
2. T=0.5s: Assistant writes "I've updated the file..." → file grows
3. T=1s: Polling detects size change → High priority processing (catches message, updates size/mtime)
4. T=2s: User types response → file grows
5. T=3s: Polling detects size change → High priority processing (catches user message, updates size/mtime)

**Telemetry emission**:
- Worker batches AgentTrace events (max 100 per batch)
- Submits to `DaemonTelemetryWorkerHandle`
- Existing 3-second flush interval handles upload

## Agent Preset Integration

### Tool Use ID Extraction

**Pattern 1: Flag-based** (explicit passing)
```rust
// Checkpoint command accepts --tool-use-id flag
git-ai checkpoint claude-code --tool-use-id "toolu_xyz123"
```

**Pattern 2: Hook input extraction** (agent preset logic)
```rust
// Agent preset parses hook input JSON
impl AgentPreset for ClaudeCodePreset {
    fn extract_metadata(&self, hook_input: &str) -> PresetMetadata {
        let json: Value = serde_json::from_str(hook_input)?;
        PresetMetadata {
            session_id: json["sessionId"].as_str(),
            trace_id: json["messageId"].as_str(),
            tool_use_id: json["toolUseId"].as_str(), // NEW
            model: json["model"].as_str(),
            // ...
        }
    }
}
```

**Why both patterns**: Different agents integrate differently. Claude Code can pass structured JSON via hooks (Pattern 2). Other tools may only support command-line flags (Pattern 1). Supporting both maximizes compatibility.

**How to apply**: Checkpoint command tries Pattern 2 first (preset extraction), falls back to Pattern 1 (--tool-use-id flag), accepts null if neither available. Server-side linking works best with tool_use_id but degrades gracefully without it.

### Session Discovery

**Auto-discovery flow**:
1. Checkpoint command extracts session_id from preset
2. Queries transcripts.db for existing session
3. If not found, preset provides transcript path via `discover_transcript()` method
4. Creates session record with initial watermark
5. Notifies worker for immediate processing

**Preset method**:
```rust
pub trait AgentPreset {
    fn discover_transcript(&self, session_id: &str) -> Option<TranscriptSource>;
}

// Example: Claude Code
impl AgentPreset for ClaudeCodePreset {
    fn discover_transcript(&self, session_id: &str) -> Option<TranscriptSource> {
        let path = dirs::data_dir()?
            .join("Claude/conversations")
            .join(format!("{}.jsonl", session_id));
        
        if path.exists() {
            Some(TranscriptSource {
                path,
                format: TranscriptFormat::ClaudeJsonl,
                watermark_type: WatermarkType::ByteOffset,
            })
        } else {
            None
        }
    }
}
```

### Per-Agent Configuration

**Agent preset defines watermark strategy and transcript directories**:
```rust
pub trait AgentPreset {
    fn watermark_strategy(&self) -> WatermarkType;
    fn transcript_dirs(&self) -> Vec<PathBuf>;
    fn discover_transcript(&self, session_id: &str) -> Option<TranscriptSource>;
}

// Most agents: byte offset
impl ClaudeCodePreset {
    fn watermark_strategy(&self) -> WatermarkType {
        WatermarkType::ByteOffset
    }
    
    fn transcript_dirs(&self) -> Vec<PathBuf> {
        vec![dirs::data_dir().unwrap().join("Claude/conversations")]
    }
}

// Droid: hybrid (record + timestamp)
impl DroidPreset {
    fn watermark_strategy(&self) -> WatermarkType {
        WatermarkType::Hybrid
    }
    
    fn transcript_dirs(&self) -> Vec<PathBuf> {
        vec![dirs::data_dir().unwrap().join("Droid/transcripts")]
    }
}
```

## Error Handling & Resilience

### Transcript Processing Errors

**Categories**:
1. **Transient** - File locked, network timeout reading remote transcript
2. **Parse errors** - Malformed JSON, unexpected format changes
3. **Fatal** - File deleted, permissions changed, format completely incompatible

**Handling strategy**:
```rust
pub enum TranscriptError {
    Transient { message: String, retry_after: Duration },
    Parse { line: usize, message: String },
    Fatal { message: String },
}

impl TranscriptWorker {
    async fn process_session(&mut self, task: ProcessingTask) {
        match self.read_transcript(&task.session_id).await {
            Ok(batch) => {
                self.emit_events(batch.events).await;
                self.update_watermark(&task.session_id, batch.new_watermark).await;
                self.clear_errors(&task.session_id).await;
            }
            Err(TranscriptError::Transient { retry_after, .. }) => {
                self.db.increment_error_count(&task.session_id);
                self.queue.push(ProcessingTask {
                    retry_count: task.retry_count + 1,
                    priority: Priority::High,
                    ..task
                });
                sleep(retry_after).await;
            }
            Err(TranscriptError::Parse { line, message }) => {
                // Log parse error, skip to next valid line
                self.db.record_error(&task.session_id, &format!("Parse error at line {}: {}", line, message));
                // Advance watermark past bad line, continue processing
            }
            Err(TranscriptError::Fatal { message }) => {
                // Mark session as failed, don't retry
                self.db.mark_session_failed(&task.session_id, &message);
            }
        }
    }
}
```

**Backoff strategy**:
- 1st retry: 5 seconds
- 2nd retry: 30 seconds  
- 3rd retry: 5 minutes
- 4th+ retry: 30 minutes
- After 20 failures: mark session as degraded (still retry but at low priority)

**Why exponential backoff**: Prevents worker from thrashing on persistent errors. Transient issues (file locks) resolve quickly. Systemic issues (permissions) need manual intervention - aggressive retries waste CPU and fill logs.

**How to apply**: Worker tracks retry_count per task. Each failure increments count and calculates next retry_after. After threshold, session marked degraded and moved to low priority queue (still retries but doesn't block other work).

### Database Corruption

**SQLite WAL mode**: Enable write-ahead logging for transcripts.db
- Prevents corruption from daemon crashes
- Allows concurrent reads during processing

**Schema validation**: On startup, verify schema version
- If version mismatch, attempt migration
- If migration fails, recreate database (lose watermarks, will reprocess)

### Worker Crash Recovery

**State persistence**: All state in transcripts.db
- Worker crash = lose in-flight processing only
- On restart, rediscover sessions and resume from last watermark
- May emit duplicate AgentTrace events (server deduplicates via trace_id)

**Graceful shutdown**: Daemon shutdown signal
- Worker drains priority queue (process Immediate tasks)
- Flushes any batched events
- Closes database cleanly

## Offline Resilience

### Core Principle

All transcript processing and telemetry emission works offline-first. Network unavailability never blocks local development or checkpoint recording. Like git, git-ai assumes hardware may be disconnected from the network for unpredictable periods.

**Why offline-first**: Developers work on planes, trains, coffee shops with flaky WiFi, or on secure networks without internet. Local development workflow must never depend on network connectivity.

**How to apply**: Worker processes transcripts using local files only. Telemetry upload failures queue locally. When network returns, backlog uploads automatically. No user intervention needed.

### Offline Behavior

**Transcript processing**: Continues normally
- Worker reads local transcript files
- Extracts AgentTrace events
- Stores in local metrics.db
- Updates watermarks in transcripts.db
- All local state remains consistent

**Telemetry emission**: Graceful degradation
- `upload_metrics_with_retry()` already handles network failures
- Failed uploads stay queued in metrics.db
- Worker continues processing, metrics accumulate locally
- When network returns, existing flush logic uploads backlog

**No special offline mode**: Worker doesn't detect or care about network state
- Separation of concerns: worker processes, telemetry_worker uploads
- Network failures handled at upload layer, not processing layer

### Storage Limits

**Metrics database growth**: Bounded by existing retention policy
- Metrics.db already has size limits and cleanup logic
- Long offline periods: oldest events dropped per existing policy
- Critical: watermark updates and local state never lost

**Transcripts database**: Minimal storage
- Only watermarks and metadata (bytes, not message content)
- Grows with number of unique sessions, not transcript size
- Expected size: <10MB for thousands of sessions

### Sync After Reconnection

**Automatic**: No user intervention needed
- Telemetry worker's existing 3-second flush continues
- Backlog uploads gradually over time
- Rate limiting and retry logic already in place

**Priority**: Live events processed first
- Worker always prioritizes Immediate tasks (new checkpoints)
- Historical backfill pauses if offline period created large backlog
- Ensures current development isn't blocked by upload catchup

## Testing Strategy

### Unit Tests

**Transcripts module**:
- Watermark serialization/deserialization
- Each format reader with fixtures (sample JSONL files)
- TranscriptsDatabase CRUD operations
- Error handling (malformed JSON, missing files)

**Metrics changes**:
- EventAttributes with session_id/trace_id serialization
- CheckpointValues with tool_use_id roundtrip
- Backward compatibility (old events without new fields)

### Integration Tests

**Worker lifecycle**:
- Startup migration from internal_db
- Session discovery and queue population
- Priority ordering (Immediate > High > Low)
- Graceful shutdown

**End-to-end flow**:
1. Create test repo with checkpoints
2. Mock transcript files for known agents
3. Verify worker processes transcripts
4. Assert AgentTrace events emitted
5. Check watermarks advanced

**Offline scenarios**:
- Telemetry upload fails (network down)
- Verify events queue locally
- Simulate network return
- Assert backlog uploads

### Snapshot Tests

- AgentTrace event serialization for each agent format
- Watermark values for each strategy type
- Transcripts.db schema dumps

## Implementation Phases

### Phase 1: Foundation (Week 1)
- Create `src/transcripts/` module with basic structure
- Implement WatermarkStrategy + ByteOffset/Hybrid implementations
- Create TranscriptsDatabase schema and wrapper
- Add session_id/trace_id to EventAttributes
- Write unit tests for watermark and database

### Phase 2: Transcript Readers (Week 2)
- Move Claude Code reader from `src/commands/checkpoint_agent/transcript_readers.rs` to `src/transcripts/formats/claude.rs`
- Update reader to use new watermark abstraction
- Port other high-priority readers (Cursor, Droid, Copilot) to `src/transcripts/formats/`
- Update all readers to emit AgentTraceValues
- Add reader unit tests with fixtures

### Phase 3: Worker (Week 3)
- Implement TranscriptWorker with priority queue and polling loop
- Add daemon integration (spawn worker, control socket)
- Implement checkpoint notification flow with deduplication
- Implement modification detection via file metadata polling
- Add internal_db migration logic
- Integration tests for worker lifecycle and deduplication

### Phase 4: Agent Integration (Week 4)
- Update agent presets to extract tool_use_id
- Add discover_transcript() to presets
- Update checkpoint command to populate new fields
- Add tool_use_id to CheckpointValues
- End-to-end tests with real agents

### Phase 5: Polish & Migration (Week 5)
- Performance tuning (batch sizes, intervals)
- Error handling refinement (backoff tuning)
- Migration testing (upgrade from internal_db)
- Documentation updates
- Monitor telemetry in staging environment

## Success Criteria

- [ ] All transcript readers moved to `src/transcripts/` module
- [ ] Internal_db deprecated, all new installs use transcripts.db
- [ ] Worker processes new checkpoints within 100ms (Immediate priority)
- [ ] Polling detects trailing messages within 1 second (High priority)
- [ ] Historical transcripts process incrementally without blocking (Low priority)
- [ ] Deduplication prevents duplicate work from checkpoint + file events
- [ ] Offline operation works (network failures don't block checkpoints)
- [ ] Metrics events include session_id/trace_id/tool_use_id
- [ ] Server-side can link checkpoints to transcript tool calls
- [ ] Trailing assistant messages (after last tool call) are captured
- [ ] All existing tests pass
- [ ] New integration tests cover worker lifecycle, polling, and deduplication
- [ ] Zero regressions in checkpoint recording performance
