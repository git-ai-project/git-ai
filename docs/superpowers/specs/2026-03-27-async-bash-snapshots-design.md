# Async Captured Checkpoints for Bash Tool Hooks

**Date:** 2026-03-27
**Status:** Approved
**PR Target:** johnw/bash-support (PR #798)

## Problem

When a bash tool call runs in an AI agent session, the system takes stat-diff snapshots (lstat metadata before/after) to detect which files changed. The resulting checkpoint is submitted to the daemon for processing. Two problems arise:

1. **Blocking latency:** Live checkpoints require the daemon to process synchronously, adding latency to every bash tool call.
2. **Race condition with queued checkpoints:** If earlier checkpoints are still queued in the daemon, files may have been modified by prior operations whose checkpoints haven't been processed yet. The bash tool's post-snapshot would incorrectly attribute those earlier changes to the bash command.

## Solution

Convert the bash tool's checkpoint flow to use the **captured (async) checkpoint** pattern that already exists for scoped file-edit tools. The key additions:

1. A new daemon API to query per-file mtime watermarks (last-snapshotted mtime per file).
2. Pre-hook content capture of files that changed since the daemon's last checkpoint.
3. Post-hook content capture of files the bash command modified.
4. Both submitted as fire-and-forget captured checkpoints.

## Approach

**Approach A (selected):** Daemon watermark query + two captured checkpoints per bash call. The daemon exposes watermarks, the CLI captures content, and submits async. Chosen over:
- Approach B (post-hook only upgrade): doesn't solve the inter-checkpoint race.
- Approach C (daemon-side orchestration): too much daemon complexity, harder to test.

## Design

### 1. Daemon Watermark Query API

**New control request** in `src/daemon/control_api.rs`:

```rust
// method: "snapshot.watermarks"
ControlRequest::SnapshotWatermarks {
    repo_working_dir: String,
}
```

**Response:**
```rust
ControlResponse {
    ok: true,
    data: Some(json!({
        "watermarks": { "src/main.rs": 1711234567890000000_u128, ... }
    })),
}
```

Returns a `HashMap<String, u128>` mapping relative file paths to `last_checkpoint_mtime_ns`.

**Daemon-side state** (in `src/daemon.rs` or `src/daemon/domain.rs`):

Add `file_snapshot_watermarks: HashMap<String, u128>` to per-family state. Updated during `apply_checkpoint_side_effect()` — when a checkpoint processes file X, set `watermarks[X] = X.mtime_at_checkpoint_time`.

**Staleness is safe:** If unprocessed checkpoints are queued, the watermarks will be behind reality. This causes the pre-hook to capture MORE files than necessary (conservative). The daemon's ordered queue ensures correct final state regardless.

**Handler:** Route `SnapshotWatermarks` through the existing `handle_control_request()` dispatcher. The handler resolves the family key and reads watermarks via a new `FamilyMsg::GetWatermarks` message to the family actor (following the same pattern as `FamilyMsg::Status`). This ensures thread-safe access since all `FamilyState` reads/writes go through the actor's message channel.

**Serialization:** The new `file_snapshot_watermarks` field on `FamilyState` must use `#[serde(default)]` to maintain backward compatibility with persisted state.

**Timeout:** The watermark query uses a tight 500ms timeout (not the 300s checkpoint timeout). On failure, the pre-hook proceeds without content capture (graceful degradation).

### 2. Bash Tool Content Capture

Extend `handle_bash_tool` in `src/commands/checkpoint_agent/bash_tool.rs`.

**New types:**

```rust
pub struct BashToolResult {
    pub action: BashCheckpointAction,
    pub captured_checkpoint: Option<CapturedCheckpointInfo>,
}

pub struct CapturedCheckpointInfo {
    pub capture_id: String,
    pub repo_working_dir: String,
}
```

**Mtime conversion:** `StatEntry.mtime` is `Option<SystemTime>`. Watermarks are `u128` nanoseconds since epoch. A helper function converts between them:

```rust
fn system_time_to_nanos(t: SystemTime) -> u128 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
```

Comparison uses a grace window of 2 seconds (matching `_MTIME_GRACE_WINDOW_SECS`) to handle low-resolution filesystems (HFS+ has 1s granularity): `mtime_ns > watermark + GRACE_WINDOW_NS` means the file has changed.

**Pre-hook flow (daemon mode):**

1. Clean up stale snapshots (existing).
2. Take stat-snapshot (existing).
3. Save stat-snapshot to disk (existing).
4. **Query daemon for watermarks** via `send_control_request(SnapshotWatermarks { repo_working_dir })`. On failure (timeout, no daemon), skip steps 5-7.
5. **Find files where `system_time_to_nanos(stat_entry.mtime) > watermark[file] + GRACE_WINDOW_NS`** — these changed since the daemon's last checkpoint.
6. **Filter to git-dirty files only** — use the tracked_files set from the stat-snapshot, plus any untracked files that pass gitignore.
7. **If any stale files found:** read their content into a `HashMap<String, String>`, then construct a synthetic `AgentRunResult` with `will_edit_filepaths: Some(stale_file_paths)` and `dirty_files: Some(content_map)`. Call `prepare_captured_checkpoint()` with `CheckpointKind::Human`. The synthetic result provides the file list that `explicit_capture_target_paths()` needs, and `dirty_files` provides the inline content so the capture pipeline reads from memory rather than re-reading from disk.
8. Return `BashToolResult { action: TakePreSnapshot, captured_checkpoint: Some(info) }`.

**Post-hook flow (daemon mode):**

1. Load pre-snapshot, take post-snapshot, diff (existing).
2. If changes detected:
   a. **Read content of changed files into `HashMap<String, String>`.**
   b. **Construct a synthetic `AgentRunResult` with `edited_filepaths: Some(changed_paths)`, `dirty_files: Some(content_map)`, and `checkpoint_kind: CheckpointKind::AiAgent`.**
   c. **Call `prepare_captured_checkpoint()` with this result.**
3. Return `BashToolResult { action: Checkpoint(paths), captured_checkpoint: Some(info) }`.

**`handle_bash_tool` prepares but does NOT submit** the captured checkpoint. It writes the capture to disk and returns the `capture_id`. Submission to the daemon happens in the handler layer (Section 4).

**Content capture function (new):**

```rust
fn capture_file_contents(
    repo_root: &Path,
    file_paths: &[PathBuf],
) -> HashMap<String, String> {
    // Read each file, skip binary/unreadable, return path -> content
}
```

Uses `std::fs::read_to_string` with a size limit (e.g., 10 MB per file). Binary files are skipped. Errors are logged and the file is omitted from the capture (the daemon will handle the gap via its existing fallback logic).

### 3. AgentRunResult Extension

In `src/authorship/working_log.rs` (or wherever `AgentRunResult` is defined):

```rust
pub struct AgentRunResult {
    // ... existing fields ...
    pub captured_checkpoint_id: Option<String>,
}
```

When this field is `Some(capture_id)`, the handler layer submits a `CapturedCheckpointRunRequest` instead of a live checkpoint.

### 4. Preset/Handler Integration

**Agent presets** (`agent_presets.rs`, `amp_preset.rs`, `opencode_preset.rs`):

Each preset's bash tool handling changes from:

```rust
// Before
let action = handle_bash_tool(event, repo_root, session_id, tool_use_id)?;
match action {
    BashCheckpointAction::Checkpoint(paths) => { /* set edited_filepaths */ }
    ...
}
```

To:

```rust
// After
let result = handle_bash_tool(event, repo_root, session_id, tool_use_id)?;
match result.action {
    BashCheckpointAction::Checkpoint(paths) => { /* set edited_filepaths */ }
    ...
}
if let Some(info) = result.captured_checkpoint {
    agent_run_result.captured_checkpoint_id = Some(info.capture_id);
}
```

**Handler** (`git_ai_handlers.rs`):

The checkpoint dispatch logic checks `captured_checkpoint_id` BEFORE the existing `allow_captured_async` gate. The `captured_checkpoint_id` field represents an already-prepared capture that bypasses the normal live/captured decision logic:

```rust
if let Some(capture_id) = &agent_run_result.captured_checkpoint_id {
    // Already prepared by bash tool — submit directly (fire-and-forget)
    send_control_request(ControlRequest::CheckpointRun {
        request: CheckpointRunRequest::Captured(CapturedCheckpointRunRequest {
            repo_working_dir: repo_working_dir.clone(),
            capture_id: capture_id.clone(),
        }),
        wait: Some(false),
    })?;
} else if allow_captured_async { ... } else {
    // Existing live checkpoint path
}
```

**Migration note:** The existing `BashCheckpointAction` enum and its variants (`TakePreSnapshot`, `Checkpoint(paths)`, `NoChanges`, `Fallback`) remain unchanged. The new `BashToolResult` wraps the action with an optional `CapturedCheckpointInfo`. All 8+ call sites in presets change from `match handle_bash_tool(...)` to `match handle_bash_tool(...).action`, with an additional check for `.captured_checkpoint`. The non-daemon fallback path continues to use `BashCheckpointAction` variants without captured checkpoints.

### 5. Fallback Behavior

| Scenario | Behavior |
|----------|----------|
| **Non-daemon mode** | Skip watermark query. Pre-hook only takes stat-snapshot (existing). Post-hook returns `edited_filepaths` for synchronous processing. No content capture. |
| **Daemon query failure** | Log warning, proceed without pre-hook content capture. Post-hook captured checkpoint still works on its own. |
| **Large working tree (>1000 stale files)** | Skip content capture, fall back to stat-diff + live checkpoint path. |
| **File read error** | Log warning, omit file from capture. Daemon handles the gap via existing fallback. |
| **Binary/large file (>10MB)** | Skip file in capture. Daemon handles via git diff fallback. |

### 6. Testing Strategy

| Test Type | What to Test |
|-----------|-------------|
| **Unit (bash_tool.rs)** | Watermark comparison logic; file content capture; `BashToolResult` construction; stale file detection |
| **Integration (daemon)** | Full pre/post flow with real daemon; verify captured checkpoints are queued and processed in order; watermark updates after checkpoint processing |
| **Benchmark** | Daemon watermark query round-trip stays under 10ms |
| **Fallback** | Graceful degradation: daemon unavailable, query timeout, large tree, binary files |

### 7. File Change Summary

| File | Change |
|------|--------|
| `src/daemon/control_api.rs` | Add `SnapshotWatermarks` request variant |
| `src/daemon.rs` | Add watermark state to per-family data; update watermarks during `apply_checkpoint_side_effect()`; handle `SnapshotWatermarks` request |
| `src/daemon/domain.rs` | Add `file_snapshot_watermarks` field to family state (if state lives here) |
| `src/commands/checkpoint_agent/bash_tool.rs` | New `BashToolResult`, `CapturedCheckpointInfo`; content capture logic; daemon watermark query in pre-hook; captured checkpoint creation in post-hook |
| `src/commands/checkpoint_agent/agent_presets.rs` | Update all presets to handle `BashToolResult`; set `captured_checkpoint_id` |
| `src/commands/checkpoint_agent/amp_preset.rs` | Same as above for Amp preset |
| `src/commands/checkpoint_agent/opencode_preset.rs` | Same as above for OpenCode preset |
| `src/authorship/working_log.rs` | Add `captured_checkpoint_id` to `AgentRunResult` |
| `src/commands/git_ai_handlers.rs` | Check `captured_checkpoint_id` and submit captured checkpoint |
| `tests/integration/bash_tool_conformance.rs` | New tests for watermark comparison, content capture, async flow |

## Attribution Ordering

**Pre-hook attribution concern:** When the pre-hook captures files as `CheckpointKind::Human`, it may re-attribute changes that were already attributed by a prior tool's checkpoint still queued in the daemon. This is acceptable because:

1. The daemon processes checkpoints in queue order. The prior tool's checkpoint will be processed first, correctly attributing those changes to the tool.
2. The pre-hook's Human checkpoint processes second. Since the files haven't changed between the two checkpoints (the bash command hasn't run yet), the daemon will see no diff and the Human checkpoint becomes a no-op for those files.
3. The post-hook's AiAgent checkpoint processes third, correctly attributing the bash command's changes.

The over-capture in step 2 is redundant work but produces correct final attribution.

## Non-Goals

- Changing the daemon's checkpoint queue ordering (already correct).
- Modifying the `prepare_captured_checkpoint()` pipeline (reused as-is).
- Supporting non-daemon mode with async checkpoints (non-daemon is being deprecated).
- Optimizing the watermark query with caching (can be added later if round-trip proves costly).
