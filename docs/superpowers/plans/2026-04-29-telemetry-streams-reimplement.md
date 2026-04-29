# Telemetry Streams Re-implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-implement telemetry-streams branch work on PR 1198 with polling-based transcript processing, unified watermarking, and enhanced telemetry linking via session_id/trace_id/tool_use_id.

**Architecture:** Three components: (1) `src/transcripts/` module with format readers, watermarking, and database, (2) `src/daemon/transcript_worker.rs` with priority queue and polling, (3) updated metrics schema with session/trace IDs for server-side linking.

**Tech Stack:** Rust 2024, tokio async, SQLite (rusqlite with WAL mode), position-encoded telemetry schema, polling-based modification detection.

**Reference:** See `docs/superpowers/specs/2026-04-29-telemetry-streams-design.md` for detailed design decisions and rationale.

---

## Phase 1: Foundation

**Files to create:**
- `src/transcripts/mod.rs` - Module declaration and exports
- `src/transcripts/types.rs` - TranscriptError, TranscriptBatch types
- `src/transcripts/watermark.rs` - WatermarkStrategy trait and implementations
- `src/transcripts/db.rs` - TranscriptsDatabase SQLite wrapper

**Files to modify:**
- `src/lib.rs` - Add transcripts module
- `src/metrics/attrs.rs` - Add session_id/trace_id fields
- `src/metrics/events.rs` - Add tool_use_id to CheckpointValues

**Tasks:**

- [ ] Create `src/transcripts/` module structure with types.rs containing TranscriptError (Transient/Parse/Fatal) and TranscriptBatch
- [ ] Implement WatermarkStrategy trait in watermark.rs with four implementations: ByteOffsetWatermark, RecordIndexWatermark, TimestampWatermark, HybridWatermark
- [ ] Add WatermarkType enum with deserialize() method for type-specific deserialization
- [ ] Implement TranscriptsDatabase in db.rs with schema: sessions table (session_id PK, agent_type, transcript_path, watermark fields, last_known_size, last_modified) and processing_stats table
- [ ] Enable WAL mode in SQLite, add migration framework with SCHEMA_VERSION
- [ ] Implement database methods: insert_session, get_session, update_watermark, update_file_metadata, all_sessions
- [ ] Add session_id (position 24, required) and trace_id (position 25, nullable) to EventAttributes in src/metrics/attrs.rs
- [ ] Tombstone position 22 (old prompt_id) with comment - never reuse this index
- [ ] Add tool_use_id (position 7, nullable) to CheckpointValues in src/metrics/events.rs
- [ ] Write unit tests for watermark serialization/deserialization and round-trip through WatermarkType
- [ ] Write unit tests for TranscriptsDatabase CRUD operations
- [ ] Write unit tests for EventAttributes and CheckpointValues with new fields
- [ ] Update all existing EventAttributes constructions to include session_id (use empty string as placeholder for now)
- [ ] Run `cargo test --lib` to verify foundation compiles and tests pass
- [ ] Commit phase 1 changes

---

## Phase 2: Transcript Readers

**Files to create:**
- `src/transcripts/processor.rs` - Format dispatch logic
- `src/transcripts/formats/mod.rs` - Format module
- `src/transcripts/formats/claude.rs` - Claude Code JSONL reader
- `src/transcripts/formats/cursor.rs` - Cursor JSONL reader
- `src/transcripts/formats/droid.rs` - Droid JSONL reader (uses Hybrid watermark)
- `src/transcripts/formats/copilot.rs` - Copilot session/event-stream readers

**Files to modify:**
- `src/transcripts/mod.rs` - Export processor and formats

**Files to reference:**
- `src/commands/checkpoint_agent/transcript_readers.rs` - Old implementations to port

**Tasks:**

- [ ] Create processor.rs with TranscriptFormat enum (ClaudeJsonl, CursorJsonl, DroidJsonl, CopilotSessionJson, CopilotEventStreamJsonl)
- [ ] Implement process_transcript() dispatcher that matches on format and calls appropriate reader
- [ ] Create formats/ module structure with mod.rs declaring submodules
- [ ] Port Claude Code reader from checkpoint_agent/transcript_readers.rs to formats/claude.rs
- [ ] Update Claude reader to use ByteOffsetWatermark and return TranscriptBatch with AgentTraceValues
- [ ] Implement read_incremental() for Claude: open file, seek to watermark offset, read JSONL lines, extract events, advance watermark
- [ ] Port Cursor reader to formats/cursor.rs (similar to Claude, uses ByteOffset)
- [ ] Port Droid reader to formats/droid.rs using HybridWatermark (offset + record + timestamp)
- [ ] Port Copilot readers to formats/copilot.rs (session JSON and event-stream JSONL)
- [ ] Create test fixtures directory with sample transcript files for each format
- [ ] Write unit tests for each reader: read from fixture, verify events extracted, verify watermark advanced
- [ ] Test watermark resume: read partial file, save watermark, append to file, read again from watermark
- [ ] Test error handling: malformed JSON, missing fields, file not found
- [ ] Run `cargo test --lib transcripts` to verify readers work
- [ ] Commit phase 2 changes

---

## Phase 3: Transcript Worker

**Files to create:**
- `src/daemon/transcript_worker.rs` - TranscriptWorker implementation

**Files to modify:**
- `src/daemon/mod.rs` - Export transcript_worker
- `src/daemon.rs` - Spawn TranscriptWorker on daemon startup
- `src/daemon/control_api.rs` - Add checkpoint_recorded message type

**Tasks:**

- [ ] Create TranscriptWorker struct with: transcripts_db, priority_queue, in_flight HashSet, telemetry_handle, shutdown_signal
- [ ] Define ProcessingTask struct with: key (canonical path), session_id, priority (Immediate/High/Low), retry_count
- [ ] Implement priority queue with Immediate > High > Low ordering
- [ ] Implement spawn() method that initializes worker, migrates internal_db if exists, discovers sessions, starts processing loop
- [ ] Implement internal_db migration: read prompts from ~/.git-ai/internal.db, create session records in transcripts.db for known transcripts, rename to internal.db.deprecated
- [ ] Implement session discovery: scan transcript directories from AgentPresets, create session records for unknown transcripts
- [ ] Implement processing loop with tokio::select: 100ms tick for queue processing, 1s tick for modification detection, checkpoint notification receiver
- [ ] Implement detect_transcript_modifications(): query all sessions, stat files, compare size/mtime against database, queue modified sessions at High priority
- [ ] Implement checkpoint notification handler: extract transcript_path, canonicalize, deduplicate via in_flight set, queue at Immediate priority
- [ ] Implement process_session(): call process_transcript(), emit AgentTrace events to telemetry_handle, update watermark and file metadata, handle errors with exponential backoff
- [ ] Implement error handling: Transient errors retry with backoff (5s, 30s, 5m, 30m), Parse errors skip and log, Fatal errors mark session failed
- [ ] Add checkpoint_recorded message to control_api.rs with session_id, trace_id, transcript_path fields
- [ ] Wire TranscriptWorker spawn into daemon.rs startup sequence after telemetry worker
- [ ] Add graceful shutdown: drain Immediate priority tasks, flush events, close database
- [ ] Write integration test: create test repo, mock transcript file, fire checkpoint, verify worker processes and emits events
- [ ] Write integration test: modify transcript file, verify polling detects change within 1 second
- [ ] Write integration test: fire checkpoint + modify file, verify deduplication (only processes once)
- [ ] Run `cargo test` to verify worker tests pass
- [ ] Commit phase 3 changes

---

## Phase 4: Agent Integration

**Files to modify:**
- `src/commands/checkpoint.rs` - Extract tool_use_id, send notification to daemon
- `src/commands/checkpoint_agent/presets.rs` - Add AgentPreset trait methods

**Agent presets to update:**
- Claude Code preset
- Cursor preset  
- Droid preset
- Copilot preset
- (Others as needed)

**Tasks:**

- [ ] Add transcript_dirs() method to AgentPreset trait returning Vec<PathBuf>
- [ ] Add discover_transcript() method to AgentPreset trait returning Option<TranscriptSource>
- [ ] Add watermark_strategy() method to AgentPreset trait returning WatermarkType
- [ ] Update ClaudeCodePreset to implement new methods: transcript_dirs() returns Claude/conversations, watermark_strategy() returns ByteOffset
- [ ] Update CursorPreset to implement new methods
- [ ] Update DroidPreset to implement new methods: watermark_strategy() returns Hybrid
- [ ] Update CopilotPreset to implement new methods
- [ ] Update checkpoint.rs to extract tool_use_id from preset metadata (hook input JSON or --tool-use-id flag)
- [ ] Populate CheckpointValues.tool_use_id and EventAttributes.trace_id with tool_use_id
- [ ] Populate EventAttributes.session_id from preset metadata
- [ ] After recording checkpoint, send checkpoint_recorded notification to daemon via control socket with session_id, trace_id, transcript_path
- [ ] Query transcripts.db for session by session_id; if not found, call preset.discover_transcript() and create session record
- [ ] Write test: mock checkpoint with tool_use_id, verify notification sent to daemon
- [ ] Write test: checkpoint for new session, verify session discovered and created
- [ ] Write end-to-end test: checkpoint → worker processes → AgentTrace events emitted with correct session_id/trace_id
- [ ] Run `cargo test` to verify agent integration works
- [ ] Commit phase 4 changes

---

## Phase 5: Polish & Migration

**Files to modify:**
- `src/authorship/internal_db.rs` - Mark as deprecated
- Various files - Replace EventAttributes constructions with real session_id values

**Documentation to update:**
- CHANGELOG.md
- Any user-facing docs about telemetry

**Tasks:**

- [ ] Review all EventAttributes constructions and populate session_id from actual session context (not empty string placeholders)
- [ ] Add deprecation comment to internal_db.rs indicating transcripts.db has replaced it
- [ ] Test migration: create ~/.git-ai/internal.db with test data, start daemon, verify migration runs and renames to internal.db.deprecated
- [ ] Test offline mode: disconnect network, fire checkpoints, verify transcripts process and events queue locally
- [ ] Test graceful degradation: make transcript unreadable, verify session marked failed and doesn't block other sessions
- [ ] Performance test: create 1000 session records, verify polling loop processes in <10ms per iteration
- [ ] Review all metrics event emission sites to ensure session_id/trace_id populated correctly
- [ ] Run full test suite: `task test`
- [ ] Run lint and format: `task lint && task fmt`
- [ ] Manual testing: fire checkpoints from Claude Code, verify events appear in metrics.db with session_id/trace_id/tool_use_id
- [ ] Manual testing: write messages after checkpoint, verify trailing messages captured within 1 second
- [ ] Update CHANGELOG.md with telemetry improvements
- [ ] Commit phase 5 changes
- [ ] Create PR description summarizing changes, link to design spec, highlight breaking changes (internal_db deprecated)

---

## Success Criteria

After completing all phases, verify:

- [ ] All transcript readers moved to `src/transcripts/` module
- [ ] Internal_db deprecated, all new installs use transcripts.db  
- [ ] Worker processes new checkpoints within 100ms (Immediate priority)
- [ ] Polling detects trailing messages within 1 second (High priority)
- [ ] Historical transcripts process incrementally without blocking (Low priority)
- [ ] Deduplication prevents duplicate work from checkpoint + file events
- [ ] Offline operation works (network failures don't block checkpoints)
- [ ] Metrics events include session_id/trace_id/tool_use_id
- [ ] Server-side can link checkpoints to transcript tool calls via (session_id, trace_id)
- [ ] Trailing assistant messages (after last tool call) are captured
- [ ] All existing tests pass
- [ ] New integration tests cover worker lifecycle, polling, and deduplication
- [ ] Zero regressions in checkpoint recording performance
- [ ] `task lint && task fmt` passes
- [ ] Manual testing confirms end-to-end flow works
