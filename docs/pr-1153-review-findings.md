# PR 1153 Review: Sessions V2 & Trace IDs

**Reviewer**: Claude Opus 4.6  
**Date**: 2026-05-05  
**Scope**: 293 files, ~54K insertions/deletions

---

## Critical Findings

### C1: `convert_to_checkpoints_for_squash` crashes on session-format attestations

**File**: `src/authorship/authorship_log_serialization.rs:432-442`

The function handles `h_`-prefixed attestation hashes (skips them) and bare hex hashes
(looks them up in `self.metadata.prompts`). However, it does NOT handle `s_`-prefixed
session attestation hashes. When a session-format entry (e.g., `s_abc123::t_xyz789`) is
encountered, the code falls through to `self.metadata.prompts.get(session_hash)` which
returns `None`, triggering an `Err("Missing prompt record for hash: s_abc123::t_xyz789")`.

**Impact**: Any repository using the new session format that undergoes a merge --squash
operation will error out. While `convert_to_checkpoints_for_squash` is currently only
called from unit tests (not a production path in the daemon's `prepare_working_log_after_squash_from_final_state`),
it is a public API that will be hit when the squash merge path uses it.

**Fix**: Add `s_` prefix handling that looks up the session key in `self.metadata.sessions`
and converts to a PromptRecord via `session_record.to_prompt_record()`, or skips session
entries similarly to `h_` entries if sessions should be treated as opaque during squash.

---

### C2: OpenCode agent `read_incremental` has no batch size limit — unbounded memory

**File**: `src/transcripts/agents/opencode.rs:47-108`

The SQL queries have no `LIMIT` clause. All messages with `time_updated > watermark` are
loaded at once. The `batch_size_hint()` is defined (returns 200) but never applied to
the SQL queries. For sessions with thousands of messages, this loads unbounded data into
memory.

**Impact**: OOM crash on large OpenCode databases. The `batch_size_hint()` contract
expected by the transcript worker loop is violated.

**Fix**: Add `LIMIT ?` to the SQL query using `self.batch_size_hint()`.

---

### C3: OpenCode agent timestamp watermark can skip messages sharing same millisecond

**File**: `src/transcripts/agents/opencode.rs:232-265`

The query uses `time_updated > watermark_millis` (strict greater-than) and sets the new
watermark to `max(time_updated)`. Messages sharing the exact same millisecond timestamp
as the boundary will be permanently skipped on subsequent calls.

**Impact**: Permanent data loss for messages that share a millisecond timestamp with
the watermark boundary.

**Fix**: Use `>=` in the query and deduplicate by message ID, or include message ID
as secondary watermark component.

---

## High Findings

### H1: `mutate_all_checkpoints` in `post_commit` is a no-op that wastes I/O

**File**: `src/authorship/post_commit.rs:94-97`

`update_prompts_to_latest` builds a `HashMap` of checkpoint indices but then does nothing
with it (comment says "Transcript enrichment disabled"). Yet `mutate_all_checkpoints`
reads the entire JSONL file, calls the no-op mutator, and writes it back unchanged.

**Impact**: Unnecessary file I/O on every commit. For repos with many checkpoints, this
adds latency to the post-commit path.

**Fix**: Replace `mutate_all_checkpoints` call with `read_all_checkpoints` since no
mutation occurs.

---

### H2: `wrapper_states` HashMap entries leak when trace events never arrive

**File**: `src/daemon.rs:7488-7511`

When a wrapper sends a pre-state via `store_wrapper_state` but the corresponding git
command produces no trace2 events (e.g., trace socket disconnected), the entry is never
consumed by `apply_wrapper_state_overlay` and `gc_stale_family_state` does not clean up
`wrapper_states`.

**Impact**: Slow memory growth over daemon lifetime. Each leaked entry is small (two
`Option<RepoContext>` + timestamp), but over hours/days on busy machines with occasional
trace failures, this could accumulate.

**Fix**: Add `wrapper_states` cleanup to `gc_stale_family_state` — remove entries whose
`received_at_ns` is older than a threshold (e.g., 60 seconds).

---

### H3: `sweep_coordinator.run_sweep()` blocks tokio runtime with synchronous I/O

**File**: `src/daemon/transcript_worker.rs:170-174`

The `run_sweep` async method calls `self.sweep_coordinator.run_sweep()` synchronously,
which performs filesystem I/O (directory scanning for all agents, `fs::metadata` calls,
database queries). This blocks the tokio worker thread.

**Impact**: During a sweep, checkpoint notifications won't be processed. Typically fast
but could be seconds on slow filesystems.

**Fix**: Wrap in `tokio::task::spawn_blocking()`.

---

### H4: `enqueue_trace_payload` may panic from non-tokio thread under backpressure

**File**: `src/daemon.rs:4911-4916`

When the trace ingest channel is full, the code checks `tokio::runtime::Handle::try_current().is_ok()`.
Trace handler threads are spawned via `std::thread::spawn` (line 7806). On these threads,
`try_current()` returns `Err` (no runtime context in TLS), so the `else` branch with plain
`tx.blocking_send(payload)` is taken. This is correct — `blocking_send` works from any thread.

**After re-analysis**: This is actually safe. The `try_current()` check correctly distinguishes
tokio worker threads (where `block_in_place` is valid) from OS threads (where `blocking_send`
is used directly). **Downgrading from the initial agent finding.**

**Revised Severity**: Low (no actual bug).

---

### H5: `drain_immediate_tasks` swallows processing errors during shutdown

**File**: `src/daemon/transcript_worker.rs:486-488`

During shutdown drain, `Ok(Err(e))` (processing errors) from `spawn_blocking` is silently
ignored — only panics (`Err(e)`) are logged. Processing errors for immediate-priority
tasks are lost.

**Impact**: Debugging failures during daemon shutdown is harder because errors aren't
recorded in the transcripts DB.

**Fix**: Match on `Ok(Err(e))` and log/record the error.

---

### H6: `format!("{:?}")` used for `watermark_type` and `transcript_format` serialization

**File**: `src/daemon/sweep_coordinator.rs:104-105`

Debug format is used to serialize enum variants to the database. `FromStr` is used for
deserialization. Currently these happen to match, but this is a fragile coincidence.

**Impact**: Future refactoring (adding data to enum variants) would silently break
deserialization, causing sessions to fail processing with a parse error.

**Fix**: Use `Display`/`to_string()` instead of `Debug` format.

---

### H7: No fallback authorship production when daemon is unavailable

**File**: `src/commands/git_handlers.rs:107-115`

When the daemon is not connected, the wrapper does a plain proxy with no invocation_id.
Commits made while the daemon is down will have zero authorship data — no notes produced.

**Impact**: Complete authorship data loss for commits during daemon downtime. The old
synchronous model guaranteed note production. This is an architectural tradeoff of the
daemon model, not a bug per se.

**Severity re-assessment**: This is design-level. The daemon model trades reliability
for performance. Mitigated by daemon auto-restart. Keeping as High for visibility.

---

## Medium Findings

### M1: Double read of checkpoints JSONL during checkpoint execution

**File**: `src/daemon/checkpoint.rs:180, 309`

`execute_resolved_checkpoint` reads all checkpoints at line 180, then `append_checkpoint`
reads them again at line 309 (repo_storage.rs:393). For large checkpoint histories, this
doubles I/O.

**Fix**: Pass the already-read checkpoints to `append_checkpoint` or restructure to
avoid the double read.

---

### M2: Unbounded channel for transcript checkpoint notifications

**File**: `src/daemon/transcript_worker.rs:63, 499`

`UnboundedSender<CheckpointNotification>` has no backpressure. Under heavy AI agent
activity, this channel can grow without limit.

**Impact**: Memory growth under sustained load, unlike the trace ingest queue which
caps at 16,384 with backpressure.

---

### M3: Recursive directory scanning follows symlinks without depth limit

**Files**: `src/transcripts/agents/claude.rs:58`, `droid.rs:46`, `codex.rs:95`

`scan_jsonl_recursive()` and similar functions follow symlinks. A symlink loop would
cause infinite recursion and stack overflow.

---

### M4: Foreign key constraint not enforced in transcripts DB

**File**: `src/transcripts/db.rs:41-45`

`PRAGMA foreign_keys = ON` is never set. The FK on `processing_stats` is documentation only.

---

### M5: Trace ID discrepancy between orchestrator and execution

**File**: `src/daemon/checkpoint.rs:237` vs `src/commands/checkpoint_agent/orchestrator.rs:191`

The orchestrator generates a `trace_id` and puts it in `CheckpointRequest.trace_id`. The
daemon execution generates a NEW `trace_id` for the attestation key and checkpoint field.
The orchestrator's trace_id is only used for transcript worker correlation.

**Impact**: No data loss, but makes end-to-end tracing more complex than necessary. Two
different trace IDs exist for the same logical checkpoint call.

---

### M6: Secondary runtime blocking nested inside tokio async context

**File**: `src/daemon/checkpoint.rs:240, 409`

Checkpoint execution used a secondary runtime for concurrent file processing. This
blocked the tokio worker thread. The daemon serializes checkpoints per-family so only
one is blocked at a time per repo, but multiple repos can saturate the thread pool.

---

### M7: Silent truncation of files over MAX_CHECKPOINT_FILES (1000)

**File**: `src/commands/checkpoint_agent/orchestrator.rs:82-89`

Files beyond the 1000 limit are silently dropped with only a `tracing::warn`. No
signal to the caller about data loss.

---

### M8: `fs::read_to_string(path).ok()` silently swallows file read errors

**File**: `src/commands/checkpoint_agent/orchestrator.rs:160`

When a file exists but can't be read (permissions, locked), the error is silently
converted to `None`, leading to dropped attribution for that file.

---

## Low Findings

### L1: `gc_stale_family_state` uses `Arc::strong_count` (TOCTOU)
### L2: Processing ticker fires every 100ms even when idle
### L3: `is_session_behind` returns true for newly-inserted sessions (redundant work)
### L4: `discover_sessions` called without timeout for slow filesystems
### L5: `notes_add_batch` has theoretical race with concurrent note writers
### L6: `resolve_alias_invocation` gated behind test-support — aliases not resolved for read-only classification
### L7: `parse_git_cli_args` called redundantly in `proxy_to_git` (short-circuits in practice)
### L8: Session records don't track per-session stats (total_additions, etc.)
### L9: `DiscoveredSession::clone()` panics via `.expect()` on watermark failure
### L10: `PosEncoded` imported but unused in transcript_worker.rs

---

## Summary

| Severity | Count | Action Required |
|----------|-------|-----------------|
| Critical | 3 | Fix before merge |
| High | 7 | Fix before merge |
| Medium | 8 | Fix if time permits |
| Low | 10 | Document/defer |

The most impactful bugs are C1 (squash path crash), C2/C3 (OpenCode agent memory/data
issues), and H1 (unnecessary I/O on every commit). The architectural concerns (H7, M6)
are design tradeoffs that should be documented rather than fixed.
