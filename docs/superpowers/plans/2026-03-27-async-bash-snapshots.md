# Async Captured Checkpoints for Bash Tool Hooks — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert the bash tool's checkpoint flow from blocking live checkpoints to fire-and-forget captured checkpoints, using daemon mtime watermarks to detect inter-checkpoint races.

**Architecture:** The daemon exposes per-file mtime watermarks via a new `snapshot.watermarks` control API. The bash tool pre-hook queries watermarks, captures content of stale files as a `CheckpointKind::Human` checkpoint, and the post-hook captures changed files as `CheckpointKind::AiAgent`. Both are submitted as async captured checkpoints through the existing `prepare_captured_checkpoint()` pipeline.

**Tech Stack:** Rust, serde, tokio (daemon actor model), interprocess (control socket)

**Spec:** `docs/superpowers/specs/2026-03-27-async-bash-snapshots-design.md`

---

## File Structure

| File | Role | Action |
|------|------|--------|
| `src/daemon/control_api.rs` | Control request/response types | Add `SnapshotWatermarks` variant |
| `src/daemon/domain.rs` | Family state types | Add `file_snapshot_watermarks` field |
| `src/daemon/family_actor.rs` | Actor message dispatch | Add `GetWatermarks` message variant |
| `src/daemon/coordinator.rs` | Request routing | Add `watermarks_family()` method |
| `src/daemon.rs` | Daemon top-level handler | Add `watermarks_for_family()` + route request |
| `src/daemon/reducer.rs` | State transitions | Update watermarks in `reduce_checkpoint()` |
| `src/commands/checkpoint_agent/bash_tool.rs` | Bash tool hook logic | Add `BashToolResult`, content capture, watermark query |
| `src/commands/checkpoint_agent/agent_presets.rs` | All preset implementations | Update 8+ call sites for `BashToolResult` |
| `src/commands/checkpoint_agent/amp_preset.rs` | Amp preset | Update 2 call sites |
| `src/commands/checkpoint_agent/opencode_preset.rs` | OpenCode preset | Update 2 call sites |
| `src/commands/git_ai_handlers.rs` | Checkpoint dispatch | Add `captured_checkpoint_id` early-return path |
| `tests/integration/bash_tool_provenance.rs` | Integration tests | Add watermark + content capture tests |

---

## Chunk 1: Daemon Watermark Infrastructure

### Task 1: Add `file_snapshot_watermarks` to `FamilyState`

**Files:**
- Modify: `src/daemon/domain.rs:248-254`

- [ ] **Step 1: Add the watermarks field to `FamilyState`**

At `src/daemon/domain.rs:248`, add a new field to the struct. The `#[serde(default)]` attribute ensures backward compatibility with persisted state that lacks this field:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyState {
    pub family_key: FamilyKey,
    pub refs: HashMap<String, String>,
    pub worktrees: HashMap<PathBuf, WorktreeState>,
    pub last_error: Option<String>,
    pub applied_seq: u64,
    #[serde(default)]
    pub file_snapshot_watermarks: HashMap<String, u128>,
}
```

- [ ] **Step 2: Add the field to the initialization in `spawn_family_actor`**

At `src/daemon/family_actor.rs:75-81`, add the new field to the `FamilyState` construction:

```rust
let mut state = FamilyState {
    family_key: family_key.clone(),
    refs: HashMap::new(),
    worktrees: HashMap::new(),
    last_error: None,
    applied_seq: 0,
    file_snapshot_watermarks: HashMap::new(),
};
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | head -20`
Expected: Success (or only unrelated warnings)

- [ ] **Step 4: Commit**

```bash
git add src/daemon/domain.rs src/daemon/family_actor.rs
git commit -m "feat: add file_snapshot_watermarks field to FamilyState"
```

### Task 2: Add `GetWatermarks` message to `FamilyMsg` and `FamilyActorHandle`

**Files:**
- Modify: `src/daemon/family_actor.rs:10-18` (enum), `src/daemon/family_actor.rs:26-56` (handle impl), `src/daemon/family_actor.rs:83-106` (actor loop)

- [ ] **Step 1: Add the `GetWatermarks` variant to `FamilyMsg`**

At `src/daemon/family_actor.rs:10-18`, add a new variant following the `Status` pattern:

```rust
pub enum FamilyMsg {
    Apply(
        Box<NormalizedCommand>,
        oneshot::Sender<Result<AppliedCommand, GitAiError>>,
    ),
    ApplyCheckpoint(oneshot::Sender<Result<ApplyAck, GitAiError>>),
    Status(oneshot::Sender<Result<FamilyStatus, GitAiError>>),
    GetWatermarks(oneshot::Sender<Result<HashMap<String, u128>, GitAiError>>),
    Shutdown,
}
```

Add `use std::collections::HashMap;` to the imports at the top of the file (it's already imported).

- [ ] **Step 2: Add the `watermarks()` method to `FamilyActorHandle`**

At `src/daemon/family_actor.rs`, after the `status()` method (line 48-56), add:

```rust
pub async fn watermarks(&self) -> Result<HashMap<String, u128>, GitAiError> {
    let (tx, rx) = oneshot::channel();
    self.tx
        .send(FamilyMsg::GetWatermarks(tx))
        .await
        .map_err(|_| {
            GitAiError::Generic("family actor watermarks send failed".to_string())
        })?;
    rx.await.map_err(|_| {
        GitAiError::Generic("family actor watermarks receive failed".to_string())
    })?
}
```

- [ ] **Step 3: Handle the message in the actor loop**

At `src/daemon/family_actor.rs:83-106`, add a match arm before `FamilyMsg::Shutdown`:

```rust
FamilyMsg::GetWatermarks(respond_to) => {
    let _ = respond_to.send(Ok(state.file_snapshot_watermarks.clone()));
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check 2>&1 | head -20`
Expected: Success

- [ ] **Step 5: Commit**

```bash
git add src/daemon/family_actor.rs
git commit -m "feat: add GetWatermarks message to family actor"
```

### Task 3: Add `SnapshotWatermarks` control request and route it

**Files:**
- Modify: `src/daemon/control_api.rs:7-35`
- Modify: `src/daemon/coordinator.rs:47-51`
- Modify: `src/daemon.rs:5984-6053`

- [ ] **Step 1: Add the `SnapshotWatermarks` variant to `ControlRequest`**

At `src/daemon/control_api.rs:7-35`, add a new variant following the `StatusFamily` pattern:

```rust
#[serde(rename = "snapshot.watermarks")]
SnapshotWatermarks { repo_working_dir: String },
```

Add this after the `StatusFamily` variant (after line 16).

- [ ] **Step 2: Add `watermarks_family()` to `Coordinator`**

At `src/daemon/coordinator.rs`, after `status_family()` (line 47-51), add:

```rust
pub async fn watermarks_family(
    &self,
    repo_working_dir: &Path,
) -> Result<HashMap<String, u128>, GitAiError> {
    let family = self.backend.resolve_family(repo_working_dir)?;
    let actor = self.get_or_create_family_actor(family).await;
    actor.watermarks().await
}
```

Add `use std::collections::HashMap;` to the imports if not already present.

- [ ] **Step 3: Add `watermarks_for_family()` to the daemon and route the request**

At `src/daemon.rs`, before `status_for_family()` (line 5984), add:

```rust
async fn watermarks_for_family(
    &self,
    repo_working_dir: String,
) -> Result<HashMap<String, u128>, GitAiError> {
    self.coordinator
        .watermarks_family(Path::new(&repo_working_dir))
        .await
}
```

At `src/daemon.rs:6004-6053`, in `handle_control_request()`, add a match arm after `StatusFamily`:

```rust
ControlRequest::SnapshotWatermarks { repo_working_dir } => self
    .watermarks_for_family(repo_working_dir)
    .await
    .and_then(|watermarks| {
        serde_json::to_value(serde_json::json!({ "watermarks": watermarks }))
            .map(|v| ControlResponse::ok(None, Some(v)))
            .map_err(GitAiError::from)
    }),
```

- [ ] **Step 4: Add watermark-specific timeout**

At `src/daemon.rs:6716-6736`, in `checkpoint_control_response_timeout()`, add a specific match arm for the watermark query before the catch-all:

```rust
ControlRequest::SnapshotWatermarks { .. } => Duration::from_millis(500),
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check 2>&1 | head -20`
Expected: Success

- [ ] **Step 6: Commit**

```bash
git add src/daemon/control_api.rs src/daemon/coordinator.rs src/daemon.rs
git commit -m "feat: add snapshot.watermarks control API with 500ms timeout"
```

### Task 4: Write unit test for watermark round-trip through family actor

**Files:**
- Modify: `src/daemon/family_actor.rs:112+` (test module)

- [ ] **Step 1: Write the test**

At `src/daemon/family_actor.rs`, in the `#[cfg(test)] mod tests` block (after existing tests), add:

```rust
#[tokio::test]
async fn test_watermarks_initially_empty() {
    let handle = spawn_family_actor(FamilyKey::new("test-family"));
    let watermarks = handle.watermarks().await.unwrap();
    assert!(watermarks.is_empty());
    handle.shutdown().await.unwrap();
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test --lib test_watermarks_initially_empty -- --nocapture 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/daemon/family_actor.rs
git commit -m "test: add watermark round-trip test for family actor"
```

### Task 5: Update watermarks during checkpoint reduction

**Files:**
- Modify: `src/daemon/reducer.rs:44-46`

Note: The current `reduce_checkpoint` function only increments `applied_seq`. Watermark updates happen when the daemon processes file-level checkpoint data. Since the reducer doesn't currently see individual file paths (it operates at the family-state level), watermarks will be updated in the daemon's `apply_checkpoint_side_effect` path rather than in the reducer. However, looking at the current code, `reduce_checkpoint` is the only mutation point for `FamilyState` during checkpoint processing.

The watermark update actually needs to happen in the bash tool's captured checkpoint path — when the daemon processes the captured checkpoint manifest, it learns which files were captured and their mtimes. This is done in the daemon's `apply_checkpoint_side_effect()` function which has access to the checkpoint payload including file paths.

For the initial implementation, we'll update watermarks through the `FamilyMsg::Apply` path using a new `FamilyMsg::UpdateWatermarks` message, called by the daemon after processing a captured checkpoint.

- [ ] **Step 1: Add `UpdateWatermarks` message variant**

At `src/daemon/family_actor.rs`, add to `FamilyMsg`:

```rust
UpdateWatermarks(HashMap<String, u128>),
```

Handle it in the actor loop:

```rust
FamilyMsg::UpdateWatermarks(new_watermarks) => {
    for (path, mtime_ns) in new_watermarks {
        let entry = state.file_snapshot_watermarks.entry(path).or_insert(0);
        if mtime_ns > *entry {
            *entry = mtime_ns;
        }
    }
}
```

Add a method to `FamilyActorHandle`:

```rust
pub async fn update_watermarks(
    &self,
    watermarks: HashMap<String, u128>,
) -> Result<(), GitAiError> {
    self.tx
        .send(FamilyMsg::UpdateWatermarks(watermarks))
        .await
        .map_err(|_| {
            GitAiError::Generic("family actor update_watermarks send failed".to_string())
        })
}
```

- [ ] **Step 2: Write a test for watermark updates**

```rust
#[tokio::test]
async fn test_watermarks_update_and_retrieve() {
    let handle = spawn_family_actor(FamilyKey::new("test-family"));

    let mut wm = HashMap::new();
    wm.insert("src/main.rs".to_string(), 1000_u128);
    wm.insert("src/lib.rs".to_string(), 2000_u128);
    handle.update_watermarks(wm).await.unwrap();

    let watermarks = handle.watermarks().await.unwrap();
    assert_eq!(watermarks.get("src/main.rs"), Some(&1000));
    assert_eq!(watermarks.get("src/lib.rs"), Some(&2000));

    // Higher mtime overwrites
    let mut wm2 = HashMap::new();
    wm2.insert("src/main.rs".to_string(), 3000_u128);
    handle.update_watermarks(wm2).await.unwrap();

    let watermarks = handle.watermarks().await.unwrap();
    assert_eq!(watermarks.get("src/main.rs"), Some(&3000));
    // Lower mtime does NOT overwrite
    assert_eq!(watermarks.get("src/lib.rs"), Some(&2000));

    handle.shutdown().await.unwrap();
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test --lib test_watermarks -- --nocapture 2>&1 | tail -15`
Expected: Both tests PASS

- [ ] **Step 4: Commit**

```bash
git add src/daemon/family_actor.rs
git commit -m "feat: add UpdateWatermarks message for mtime tracking"
```

---

## Chunk 2: Bash Tool Content Capture

### Task 6: Add `BashToolResult` and `CapturedCheckpointInfo` types

**Files:**
- Modify: `src/commands/checkpoint_agent/bash_tool.rs:163-172`

- [ ] **Step 1: Add new types after `BashCheckpointAction`**

At `src/commands/checkpoint_agent/bash_tool.rs`, after the `HookEvent` enum (line 178), add:

```rust
/// Result from `handle_bash_tool` combining the action with optional captured checkpoint info.
pub struct BashToolResult {
    /// The checkpoint action (unchanged from previous API).
    pub action: BashCheckpointAction,
    /// If set, a captured checkpoint was prepared and needs submission by the handler.
    pub captured_checkpoint: Option<CapturedCheckpointInfo>,
}

/// Info about a captured checkpoint prepared by the bash tool.
pub struct CapturedCheckpointInfo {
    pub capture_id: String,
    pub repo_working_dir: String,
}
```

- [ ] **Step 2: Add `system_time_to_nanos` helper**

At `src/commands/checkpoint_agent/bash_tool.rs`, after the constants section (line 31), add:

```rust
/// Convert a `SystemTime` to nanoseconds since UNIX epoch for watermark comparison.
fn system_time_to_nanos(t: SystemTime) -> u128 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// Grace window in nanoseconds for low-resolution filesystem mtime comparison.
const MTIME_GRACE_WINDOW_NS: u128 = (_MTIME_GRACE_WINDOW_SECS as u128) * 1_000_000_000;

/// Maximum number of stale files before skipping content capture.
const MAX_STALE_FILES_FOR_CAPTURE: usize = 1000;

/// Maximum file size for content capture (10 MB).
const MAX_CAPTURE_FILE_SIZE: u64 = 10 * 1024 * 1024;
```

- [ ] **Step 3: Add `capture_file_contents` function**

At `src/commands/checkpoint_agent/bash_tool.rs`, before `handle_bash_tool` (line 735), add:

```rust
/// Read file contents for captured checkpoint, skipping binary/large/unreadable files.
fn capture_file_contents(
    repo_root: &Path,
    file_paths: &[PathBuf],
) -> HashMap<String, String> {
    let mut contents = HashMap::new();
    for rel_path in file_paths {
        let abs_path = repo_root.join(rel_path);
        // Skip files that are too large
        match fs::metadata(&abs_path) {
            Ok(meta) if meta.len() > MAX_CAPTURE_FILE_SIZE => {
                debug_log(&format!(
                    "Skipping large file for capture: {} ({} bytes)",
                    rel_path.display(),
                    meta.len()
                ));
                continue;
            }
            Err(e) => {
                debug_log(&format!(
                    "Skipping unreadable file for capture: {}: {}",
                    rel_path.display(),
                    e
                ));
                continue;
            }
            _ => {}
        }
        match fs::read_to_string(&abs_path) {
            Ok(content) => {
                let key = crate::utils::normalize_to_posix(&rel_path.to_string_lossy());
                contents.insert(key, content);
            }
            Err(e) => {
                debug_log(&format!(
                    "Skipping non-UTF8/unreadable file for capture: {}: {}",
                    rel_path.display(),
                    e
                ));
            }
        }
    }
    contents
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check 2>&1 | head -20`
Expected: Success (types may show unused warnings, which is fine)

- [ ] **Step 5: Commit**

```bash
git add src/commands/checkpoint_agent/bash_tool.rs
git commit -m "feat: add BashToolResult types and content capture helper"
```

### Task 7: Upgrade `handle_bash_tool` to return `BashToolResult`

**Files:**
- Modify: `src/commands/checkpoint_agent/bash_tool.rs:735-834`

This is the core change. The function signature changes from returning `BashCheckpointAction` to `BashToolResult`. The existing logic remains identical, just wrapped in the new return type.

- [ ] **Step 1: Change the return type and wrap existing returns**

Change the function signature at line 735:

```rust
pub fn handle_bash_tool(
    hook_event: HookEvent,
    repo_root: &Path,
    session_id: &str,
    tool_use_id: &str,
) -> Result<BashToolResult, GitAiError> {
```

Then wrap every `Ok(BashCheckpointAction::*)` return with `Ok(BashToolResult { action: BashCheckpointAction::*, captured_checkpoint: None })`.

There are 8 return points:
- Line 756: `Ok(BashCheckpointAction::TakePreSnapshot)` → wrap
- Line 764: `Ok(BashCheckpointAction::TakePreSnapshot)` → wrap
- Line 789: `Ok(BashCheckpointAction::NoChanges)` → wrap
- Line 800: `Ok(BashCheckpointAction::Checkpoint(paths))` → wrap
- Line 811: `Ok(BashCheckpointAction::NoChanges)` → wrap
- Line 813: `Ok(BashCheckpointAction::Checkpoint(paths))` → wrap
- Line 814: `Ok(BashCheckpointAction::Fallback)` → wrap
- Line 826: `Ok(BashCheckpointAction::NoChanges)` → wrap
- Line 827: `Ok(BashCheckpointAction::Checkpoint(paths))` → wrap
- Line 828: `Ok(BashCheckpointAction::Fallback)` → wrap

Example pattern for each:
```rust
// Before:
Ok(BashCheckpointAction::TakePreSnapshot)
// After:
Ok(BashToolResult {
    action: BashCheckpointAction::TakePreSnapshot,
    captured_checkpoint: None,
})
```

- [ ] **Step 2: Verify it compiles (expect caller errors)**

Run: `cargo check 2>&1 | head -40`
Expected: Errors at all call sites in presets — this is expected and will be fixed in Task 8.

- [ ] **Step 3: Commit (compile-breaking, will be fixed in next task)**

```bash
git add src/commands/checkpoint_agent/bash_tool.rs
git commit -m "feat: upgrade handle_bash_tool to return BashToolResult

Callers updated in next commit."
```

### Task 8: Update all preset call sites

**Files:**
- Modify: `src/commands/checkpoint_agent/agent_presets.rs` (lines 161, 185, 548, 571, 1017, 1040, 2974, 2996)
- Modify: `src/commands/checkpoint_agent/amp_preset.rs` (lines 158, 180)
- Modify: `src/commands/checkpoint_agent/opencode_preset.rs` (lines 234, 256)

Each preset has a pre-hook call (ignoring return) and a post-hook call (matching on the action). The migration is mechanical:

**Pre-hook pattern (all presets):**
```rust
// Before:
let _ = bash_tool::handle_bash_tool(
    HookEvent::PreToolUse,
    repo_root,
    session_id,
    tool_use_id,
);

// After (no change needed — the `let _ =` discards the entire Result):
let _ = bash_tool::handle_bash_tool(
    HookEvent::PreToolUse,
    repo_root,
    session_id,
    tool_use_id,
);
```

Pre-hook calls already use `let _ =` so they discard the result regardless of type. No change needed.

**Post-hook pattern (all presets):**
```rust
// Before:
match bash_tool::handle_bash_tool(
    HookEvent::PostToolUse,
    repo_root,
    session_id,
    tool_use_id,
) {
    Ok(BashCheckpointAction::Checkpoint(paths)) => Some(paths),
    Ok(BashCheckpointAction::NoChanges) => None,
    ...
}

// After:
match bash_tool::handle_bash_tool(
    HookEvent::PostToolUse,
    repo_root,
    session_id,
    tool_use_id,
) {
    Ok(bash_tool::BashToolResult {
        action: BashCheckpointAction::Checkpoint(paths),
        ..
    }) => Some(paths),
    Ok(bash_tool::BashToolResult {
        action: BashCheckpointAction::NoChanges,
        ..
    }) => None,
    Ok(bash_tool::BashToolResult {
        action: BashCheckpointAction::Fallback,
        ..
    }) => None,
    Ok(bash_tool::BashToolResult {
        action: BashCheckpointAction::TakePreSnapshot,
        ..
    }) => None,
    Err(_) => None,
}
```

However, a simpler approach: destructure the result first, then match on the action:

```rust
let bash_result = bash_tool::handle_bash_tool(
    HookEvent::PostToolUse,
    repo_root,
    session_id,
    tool_use_id,
);
let edited_filepaths = match bash_result.as_ref().map(|r| &r.action) {
    Ok(BashCheckpointAction::Checkpoint(paths)) => Some(paths.clone()),
    Ok(BashCheckpointAction::NoChanges) => None,
    Ok(BashCheckpointAction::Fallback) => None,
    Ok(BashCheckpointAction::TakePreSnapshot) => None,
    Err(_) => None,
};
```

- [ ] **Step 1: Update `agent_presets.rs` — all post-hook call sites**

There are 4 preset implementations in this file with post-hook matches at approximately lines 185, 571, 1040, and 2996. For each, change the match to destructure `BashToolResult`:

```rust
// Change each post-hook match from matching BashCheckpointAction directly
// to matching via BashToolResult.action
let bash_result = bash_tool::handle_bash_tool(
    HookEvent::PostToolUse,
    repo_root,
    session_id,
    tool_use_id,
);
match bash_result.as_ref().map(|r| &r.action) {
    Ok(BashCheckpointAction::Checkpoint(paths)) => Some(paths.clone()),
    Ok(BashCheckpointAction::NoChanges) => None,
    Ok(BashCheckpointAction::Fallback) => {
        // git_status_fallback already failed inside handle_bash_tool
        None
    }
    Ok(BashCheckpointAction::TakePreSnapshot) => None,
    Err(_) => None,
}
```

- [ ] **Step 2: Update `amp_preset.rs` — post-hook call site at line 180**

Same pattern as step 1.

- [ ] **Step 3: Update `opencode_preset.rs` — post-hook call site at line 256**

Same pattern as step 1.

- [ ] **Step 4: Update imports in all three files**

Ensure `BashToolResult` is accessible. Since it's in the `bash_tool` module, the existing `use bash_tool::...` or `bash_tool::handle_bash_tool` path already works. No new imports needed if using the `bash_tool::BashToolResult` qualified path.

- [ ] **Step 5: Verify it compiles**

Run: `cargo check 2>&1 | head -20`
Expected: Success

- [ ] **Step 6: Run existing tests to verify no regressions**

Run: `cargo test --test bash_tool_provenance 2>&1 | tail -20`
Expected: All existing tests pass

- [ ] **Step 7: Commit**

```bash
git add src/commands/checkpoint_agent/agent_presets.rs src/commands/checkpoint_agent/amp_preset.rs src/commands/checkpoint_agent/opencode_preset.rs
git commit -m "refactor: update all preset call sites for BashToolResult"
```

---

## Chunk 3: Watermark Query and Pre-Hook Content Capture

### Task 9: Add daemon watermark query to pre-hook

**Files:**
- Modify: `src/commands/checkpoint_agent/bash_tool.rs:735+` (handle_bash_tool PreToolUse arm)

- [ ] **Step 1: Add imports**

At the top of `bash_tool.rs`, add:

```rust
use crate::commands::checkpoint::prepare_captured_checkpoint;
use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::daemon::control_api::ControlRequest;
```

Note: `AgentId` has fields `tool: String`, `id: String`, `model: String` (defined in `working_log.rs:43-47`). `CheckpointKind` has variants `Human`, `AiAgent`, `AiTab`.

- [ ] **Step 2: Add `query_daemon_watermarks` helper function**

Before `handle_bash_tool`, add:

```rust
/// Query the daemon for per-file mtime watermarks. Returns None on any failure.
fn query_daemon_watermarks(repo_working_dir: &str) -> Option<HashMap<String, u128>> {
    use crate::daemon::{ensure_daemon_running, send_control_request_with_timeout};
    use std::time::Duration;

    let config = ensure_daemon_running(Duration::from_millis(500)).ok()?;
    let request = ControlRequest::SnapshotWatermarks {
        repo_working_dir: repo_working_dir.to_string(),
    };
    let response = send_control_request_with_timeout(
        &config.control_socket_path,
        &request,
        Duration::from_millis(500),
    )
    .ok()?;

    if !response.ok {
        return None;
    }

    let data = response.data?;
    let watermarks_value = data.get("watermarks")?;
    serde_json::from_value::<HashMap<String, u128>>(watermarks_value.clone()).ok()
}
```

- [ ] **Step 3: Add `find_stale_files` helper**

```rust
/// Find files in the snapshot whose mtime exceeds the daemon's watermark.
fn find_stale_files(
    snapshot: &StatSnapshot,
    watermarks: &HashMap<String, u128>,
) -> Vec<PathBuf> {
    let mut stale = Vec::new();
    for (path, entry) in &snapshot.entries {
        if !entry.exists {
            continue;
        }
        let mtime_ns = entry
            .mtime
            .map(system_time_to_nanos)
            .unwrap_or(0);
        let path_str = crate::utils::normalize_to_posix(&path.to_string_lossy());
        let watermark = watermarks.get(&path_str).copied().unwrap_or(0);
        if mtime_ns > watermark + MTIME_GRACE_WINDOW_NS {
            stale.push(path.clone());
        }
    }
    stale
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check 2>&1 | head -20`
Expected: Success (unused function warnings are fine)

- [ ] **Step 5: Commit**

```bash
git add src/commands/checkpoint_agent/bash_tool.rs
git commit -m "feat: add daemon watermark query and stale file detection helpers"
```

### Task 10: Integrate watermark query into pre-hook flow

**Files:**
- Modify: `src/commands/checkpoint_agent/bash_tool.rs` (PreToolUse arm of handle_bash_tool)

- [ ] **Step 1: Add pre-hook content capture after snapshot**

Replace the `PreToolUse` arm in `handle_bash_tool`. The new flow:
1. Clean up stale snapshots (existing)
2. Take stat-snapshot (existing)
3. Save snapshot (existing)
4. Query daemon for watermarks
5. Find stale files
6. If stale files found, capture content and prepare a Human checkpoint

```rust
HookEvent::PreToolUse => {
    let _ = cleanup_stale_snapshots(repo_root);

    match snapshot(repo_root, session_id, tool_use_id) {
        Ok(snap) => {
            save_snapshot(&snap)?;
            debug_log(&format!(
                "Pre-snapshot stored for invocation {}",
                invocation_key
            ));

            // Attempt watermark-based content capture
            let captured = attempt_pre_hook_capture(repo_root, &snap);

            Ok(BashToolResult {
                action: BashCheckpointAction::TakePreSnapshot,
                captured_checkpoint: captured,
            })
        }
        Err(e) => {
            debug_log(&format!(
                "Pre-snapshot failed: {}; will use fallback on post",
                e
            ));
            Ok(BashToolResult {
                action: BashCheckpointAction::TakePreSnapshot,
                captured_checkpoint: None,
            })
        }
    }
}
```

- [ ] **Step 2: Add `attempt_pre_hook_capture` function**

```rust
/// Attempt to capture stale files detected via daemon watermarks.
/// Returns None on any failure (graceful degradation).
fn attempt_pre_hook_capture(
    repo_root: &Path,
    snap: &StatSnapshot,
) -> Option<CapturedCheckpointInfo> {
    let repo_working_dir = repo_root.to_string_lossy().to_string();
    let watermarks = query_daemon_watermarks(&repo_working_dir)?;

    let stale_files = find_stale_files(snap, &watermarks);
    if stale_files.is_empty() {
        return None;
    }

    if stale_files.len() > MAX_STALE_FILES_FOR_CAPTURE {
        debug_log(&format!(
            "Too many stale files ({}), skipping pre-hook capture",
            stale_files.len()
        ));
        return None;
    }

    debug_log(&format!(
        "Pre-hook: {} stale files detected via watermarks",
        stale_files.len()
    ));

    let contents = capture_file_contents(repo_root, &stale_files);
    if contents.is_empty() {
        return None;
    }

    let stale_paths: Vec<String> = contents.keys().cloned().collect();

    // Open the repo to call prepare_captured_checkpoint
    let repo = git2::Repository::discover(repo_root).ok()?;

    let agent_run_result = AgentRunResult {
        agent_id: AgentId {
            tool: "bash-tool".to_string(),
            id: "pre-hook".to_string(),
            model: String::new(),
        },
        agent_metadata: None,
        checkpoint_kind: CheckpointKind::Human,
        transcript: None,
        repo_working_dir: Some(repo_working_dir.clone()),
        edited_filepaths: None,
        will_edit_filepaths: Some(stale_paths),
        dirty_files: Some(contents),
        captured_checkpoint_id: None,
    };

    match prepare_captured_checkpoint(
        &repo,
        "bash-tool",
        CheckpointKind::Human,
        false,
        Some(&agent_run_result),
        false,
        None,
    ) {
        Ok(Some(capture)) => {
            debug_log(&format!(
                "Pre-hook captured checkpoint prepared: {}",
                capture.capture_id
            ));
            Some(CapturedCheckpointInfo {
                capture_id: capture.capture_id,
                repo_working_dir,
            })
        }
        Ok(None) => {
            debug_log("Pre-hook captured checkpoint: no files to capture");
            None
        }
        Err(e) => {
            debug_log(&format!("Pre-hook captured checkpoint failed: {}", e));
            None
        }
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | head -30`
Expected: Success (may need to adjust import paths)

- [ ] **Step 4: Commit**

```bash
git add src/commands/checkpoint_agent/bash_tool.rs
git commit -m "feat: integrate watermark query and content capture into pre-hook"
```

### Task 11: Add post-hook content capture

**Files:**
- Modify: `src/commands/checkpoint_agent/bash_tool.rs` (PostToolUse arm)

- [ ] **Step 1: Add `attempt_post_hook_capture` function**

```rust
/// Capture changed files detected by stat-diff as an async AiAgent checkpoint.
fn attempt_post_hook_capture(
    repo_root: &Path,
    changed_paths: &[String],
) -> Option<CapturedCheckpointInfo> {
    let repo_working_dir = repo_root.to_string_lossy().to_string();
    let path_bufs: Vec<PathBuf> = changed_paths.iter().map(PathBuf::from).collect();
    let contents = capture_file_contents(repo_root, &path_bufs);

    if contents.is_empty() {
        return None;
    }

    let repo = git2::Repository::discover(repo_root).ok()?;

    let agent_run_result = AgentRunResult {
        agent_id: AgentId {
            tool: "bash-tool".to_string(),
            id: "post-hook".to_string(),
            model: String::new(),
        },
        agent_metadata: None,
        checkpoint_kind: CheckpointKind::AiAgent,
        transcript: None,
        repo_working_dir: Some(repo_working_dir.clone()),
        edited_filepaths: Some(changed_paths.to_vec()),
        will_edit_filepaths: None,
        dirty_files: Some(contents),
        captured_checkpoint_id: None,
    };

    match prepare_captured_checkpoint(
        &repo,
        "bash-tool",
        CheckpointKind::AiAgent,
        false,
        Some(&agent_run_result),
        false,
        None,
    ) {
        Ok(Some(capture)) => {
            debug_log(&format!(
                "Post-hook captured checkpoint prepared: {}",
                capture.capture_id
            ));
            Some(CapturedCheckpointInfo {
                capture_id: capture.capture_id,
                repo_working_dir,
            })
        }
        Ok(None) => None,
        Err(e) => {
            debug_log(&format!("Post-hook captured checkpoint failed: {}", e));
            None
        }
    }
}
```

- [ ] **Step 2: Update the PostToolUse success path**

In the `PostToolUse` arm, where `Checkpoint(paths)` is returned, add captured checkpoint:

```rust
// In the success path where diff_result is non-empty:
let paths = diff_result.all_changed_paths();
debug_log(&format!(
    "Bash tool {}: {} files changed ({} created, {} modified, {} deleted)",
    invocation_key,
    paths.len(),
    diff_result.created.len(),
    diff_result.modified.len(),
    diff_result.deleted.len(),
));

let captured = attempt_post_hook_capture(repo_root, &paths);
Ok(BashToolResult {
    action: BashCheckpointAction::Checkpoint(paths),
    captured_checkpoint: captured,
})
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | head -20`
Expected: Success

- [ ] **Step 4: Run existing tests**

Run: `cargo test --test bash_tool_provenance 2>&1 | tail -20`
Expected: All existing tests pass (captured_checkpoint will be None since no daemon in tests)

- [ ] **Step 5: Commit**

```bash
git add src/commands/checkpoint_agent/bash_tool.rs
git commit -m "feat: add post-hook content capture for async checkpoints"
```

---

## Chunk 4: Handler Integration and End-to-End Wiring

### Task 12: Add `captured_checkpoint_id` to `AgentRunResult`

**Files:**
- Modify: `src/commands/checkpoint_agent/agent_presets.rs:26-35`

- [ ] **Step 1: Add the field**

```rust
pub struct AgentRunResult {
    pub agent_id: AgentId,
    pub agent_metadata: Option<HashMap<String, String>>,
    pub checkpoint_kind: CheckpointKind,
    pub transcript: Option<AiTranscript>,
    pub repo_working_dir: Option<String>,
    pub edited_filepaths: Option<Vec<String>>,
    pub will_edit_filepaths: Option<Vec<String>>,
    pub dirty_files: Option<HashMap<String, String>>,
    /// Pre-prepared captured checkpoint ID from bash tool (bypasses normal capture flow).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_checkpoint_id: Option<String>,
}
```

- [ ] **Step 2: Fix all `AgentRunResult` construction sites**

Search for `AgentRunResult {` and add `captured_checkpoint_id: None` to each. Use `cargo check` to find all sites that need updating.

Run: `cargo check 2>&1 | grep "missing field" | head -20`

Fix each one by adding the missing field.

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | head -20`
Expected: Success

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: add captured_checkpoint_id to AgentRunResult"
```

### Task 13: Wire `captured_checkpoint` from bash tool to `AgentRunResult` in presets

**Files:**
- Modify: `src/commands/checkpoint_agent/agent_presets.rs`
- Modify: `src/commands/checkpoint_agent/amp_preset.rs`
- Modify: `src/commands/checkpoint_agent/opencode_preset.rs`

At each preset's post-hook handling, after extracting `edited_filepaths` from the bash result, also extract the `captured_checkpoint`:

- [ ] **Step 1: Update preset post-hook sections**

For each preset, after the bash tool result is processed, capture the checkpoint info:

```rust
// After the edited_filepaths extraction from bash_result:
let bash_captured_checkpoint_id = bash_result
    .ok()
    .and_then(|r| r.captured_checkpoint)
    .map(|info| info.capture_id);
```

Then when constructing `AgentRunResult`:

```rust
captured_checkpoint_id: bash_captured_checkpoint_id,
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check 2>&1 | head -20`
Expected: Success

- [ ] **Step 3: Commit**

```bash
git add src/commands/checkpoint_agent/agent_presets.rs src/commands/checkpoint_agent/amp_preset.rs src/commands/checkpoint_agent/opencode_preset.rs
git commit -m "feat: wire captured_checkpoint_id from bash tool through presets"
```

### Task 14: Add early-return captured checkpoint path in handler

**Files:**
- Modify: `src/commands/git_ai_handlers.rs:1097-1099`

- [ ] **Step 1: Add captured checkpoint dispatch before existing logic**

At `src/commands/git_ai_handlers.rs`, inside `run_checkpoint_via_daemon_or_local()`, after `ensure_daemon_running` succeeds (line 1097-1098), add a check BEFORE the `allow_captured_async` gate:

```rust
Ok(config) => {
    // Check for pre-prepared captured checkpoint from bash tool
    if let Some(capture_id) = agent_run_result
        .as_ref()
        .and_then(|r| r.captured_checkpoint_id.as_ref())
    {
        let request = ControlRequest::CheckpointRun {
            request: Box::new(CheckpointRunRequest::Captured(
                CapturedCheckpointRunRequest {
                    repo_working_dir: repo_working_dir.clone(),
                    capture_id: capture_id.clone(),
                },
            )),
            wait: Some(false),
        };
        match send_control_request(&config.control_socket_path, &request) {
            Ok(response) if response.ok => {
                let estimated_files =
                    estimate_checkpoint_file_count(kind, &agent_run_result);
                return Ok(CheckpointDispatchOutcome {
                    stats: (0, estimated_files, 0),
                    queued: true,
                });
            }
            Ok(response) => {
                let message = response
                    .error
                    .unwrap_or_else(|| "unknown error".to_string());
                log_daemon_checkpoint_delegate_failure(
                    "bash_captured_request_rejected",
                    &repo_working_dir,
                    kind,
                    &message,
                );
                // Fall through to normal checkpoint path
            }
            Err(e) => {
                log_daemon_checkpoint_delegate_failure(
                    "bash_captured_connect_failed",
                    &repo_working_dir,
                    kind,
                    &e.to_string(),
                );
                // Fall through to normal checkpoint path
            }
        }
    }

    // Existing allow_captured_async logic follows...
    if allow_captured_async
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check 2>&1 | head -20`
Expected: Success

- [ ] **Step 3: Run all tests**

Run: `cargo test 2>&1 | tail -30`
Expected: All tests pass

- [ ] **Step 4: Commit**

```bash
git add src/commands/git_ai_handlers.rs
git commit -m "feat: add early-return captured checkpoint path for bash tool in handler"
```

---

## Chunk 5: Integration Tests and Cleanup

### Task 15: Add unit tests for watermark comparison and content capture

**Files:**
- Modify: `src/commands/checkpoint_agent/bash_tool.rs` (add test module or extend existing)

- [ ] **Step 1: Add unit tests at the bottom of bash_tool.rs**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_time_to_nanos() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        assert_eq!(system_time_to_nanos(t), 1_000_000_000);
    }

    #[test]
    fn test_system_time_to_nanos_epoch() {
        assert_eq!(system_time_to_nanos(SystemTime::UNIX_EPOCH), 0);
    }

    #[test]
    fn test_find_stale_files_empty_watermarks() {
        let mut entries = HashMap::new();
        entries.insert(
            PathBuf::from("src/main.rs"),
            StatEntry {
                exists: true,
                mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
                ctime: None,
                size: 100,
                mode: 0o644,
                file_type: StatFileType::File,
            },
        );
        let snap = StatSnapshot {
            entries,
            tracked_files: HashSet::new(),
            gitignore: None,
            taken_at: None,
            invocation_key: "test:test".to_string(),
            repo_root: PathBuf::from("/tmp/repo"),
        };

        let watermarks = HashMap::new();
        let stale = find_stale_files(&snap, &watermarks);
        // File has mtime > 0 (default watermark) + grace window
        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn test_find_stale_files_within_grace_window() {
        let mtime_secs = 100;
        let mut entries = HashMap::new();
        entries.insert(
            PathBuf::from("src/main.rs"),
            StatEntry {
                exists: true,
                mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs)),
                ctime: None,
                size: 100,
                mode: 0o644,
                file_type: StatFileType::File,
            },
        );
        let snap = StatSnapshot {
            entries,
            tracked_files: HashSet::new(),
            gitignore: None,
            taken_at: None,
            invocation_key: "test:test".to_string(),
            repo_root: PathBuf::from("/tmp/repo"),
        };

        // Watermark is at mtime_secs - 1 second, within the 2-second grace window
        let mut watermarks = HashMap::new();
        watermarks.insert(
            "src/main.rs".to_string(),
            (mtime_secs as u128 - 1) * 1_000_000_000,
        );
        let stale = find_stale_files(&snap, &watermarks);
        assert!(stale.is_empty(), "File within grace window should not be stale");
    }

    #[test]
    fn test_find_stale_files_beyond_grace_window() {
        let mtime_secs = 100;
        let mut entries = HashMap::new();
        entries.insert(
            PathBuf::from("src/main.rs"),
            StatEntry {
                exists: true,
                mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs)),
                ctime: None,
                size: 100,
                mode: 0o644,
                file_type: StatFileType::File,
            },
        );
        let snap = StatSnapshot {
            entries,
            tracked_files: HashSet::new(),
            gitignore: None,
            taken_at: None,
            invocation_key: "test:test".to_string(),
            repo_root: PathBuf::from("/tmp/repo"),
        };

        // Watermark is at mtime_secs - 5 seconds, beyond the 2-second grace window
        let mut watermarks = HashMap::new();
        watermarks.insert(
            "src/main.rs".to_string(),
            (mtime_secs as u128 - 5) * 1_000_000_000,
        );
        let stale = find_stale_files(&snap, &watermarks);
        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn test_find_stale_files_nonexistent_skipped() {
        let mut entries = HashMap::new();
        entries.insert(
            PathBuf::from("deleted.rs"),
            StatEntry {
                exists: false,
                mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
                ctime: None,
                size: 0,
                mode: 0,
                file_type: StatFileType::File,
            },
        );
        let snap = StatSnapshot {
            entries,
            tracked_files: HashSet::new(),
            gitignore: None,
            taken_at: None,
            invocation_key: "test:test".to_string(),
            repo_root: PathBuf::from("/tmp/repo"),
        };

        let stale = find_stale_files(&snap, &HashMap::new());
        assert!(stale.is_empty());
    }

    #[test]
    fn test_capture_file_contents_reads_text_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("hello.txt");
        fs::write(&file_path, "hello world").unwrap();

        let contents =
            capture_file_contents(dir.path(), &[PathBuf::from("hello.txt")]);
        assert_eq!(contents.get("hello.txt").unwrap(), "hello world");
    }

    #[test]
    fn test_capture_file_contents_skips_missing() {
        let dir = tempfile::tempdir().unwrap();
        let contents =
            capture_file_contents(dir.path(), &[PathBuf::from("nonexistent.txt")]);
        assert!(contents.is_empty());
    }
}
```

- [ ] **Step 2: Run the unit tests**

Run: `cargo test --lib bash_tool::tests -- --nocapture 2>&1 | tail -20`
Expected: All tests pass

- [ ] **Step 3: Commit**

```bash
git add src/commands/checkpoint_agent/bash_tool.rs
git commit -m "test: add unit tests for watermark comparison and content capture"
```

### Task 16: Run full test suite and fix any issues

- [ ] **Step 1: Run cargo clippy**

Run: `cargo clippy --all-targets 2>&1 | tail -30`
Expected: No errors (warnings are OK)

- [ ] **Step 2: Run full test suite**

Run: `cargo test 2>&1 | tail -40`
Expected: All tests pass

- [ ] **Step 3: Fix any issues found**

Address any compilation errors, test failures, or clippy warnings introduced by the changes.

- [ ] **Step 4: Final commit if fixes needed**

```bash
git add -A
git commit -m "fix: address clippy warnings and test issues"
```

### Task 17: Create PR targeting johnw/bash-support

- [ ] **Step 1: Push the branch**

```bash
git push -u origin HEAD
```

- [ ] **Step 2: Create the PR**

```bash
gh pr create \
  --base johnw/bash-support \
  --title "feat: async captured checkpoints for bash tool hooks" \
  --body "$(cat <<'EOF'
## Summary

- Adds daemon `snapshot.watermarks` API to query per-file mtime watermarks
- Pre-hook captures content of stale files (mtime > watermark + grace) as `Human` checkpoint
- Post-hook captures bash command's changed files as `AiAgent` checkpoint
- Both submitted as fire-and-forget captured checkpoints via existing pipeline
- Graceful degradation: non-daemon mode, query failures, large trees all fall back safely

## Design

See `docs/superpowers/specs/2026-03-27-async-bash-snapshots-design.md`

## Test plan

- [ ] Unit tests for `system_time_to_nanos`, `find_stale_files`, `capture_file_contents`
- [ ] Family actor watermark round-trip tests
- [ ] All existing `bash_tool_provenance` integration tests pass
- [ ] Full `cargo test` suite passes
- [ ] `cargo clippy` clean

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 3: Report PR URL**
