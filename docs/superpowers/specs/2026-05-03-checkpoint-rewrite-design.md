# Checkpoint Rewrite: First-Principles End-to-End Simplification

## Problem

The checkpoint CLI command (`handle_checkpoint`) is ~580 lines with another ~240 in `run_checkpoint_via_daemon_or_local`. It has accumulated: captured checkpoint blob ceremony, dirty_files branching, sync fallback paths, unscoped checkpoint support, multi-repo detection, cross-repo dispatch, and filesystem-based bash snapshot storage. These interleaved concerns make the code difficult to reason about and maintain.

## Goals

- `handle_checkpoint` becomes a ~50-line thin dispatcher: parse preset, build request, send to daemon
- All processing (diffing, attribution, metrics, author identity) happens daemon-side
- No unscoped checkpoints — everything is scoped or bash (bash produces scoped checkpoints)
- No filesystem-based captured checkpoint blobs or bash snapshot files
- Absolute file paths enforced at ingestion — hard error if relative
- No sync fallback — daemon send failure is a hard fail (log + exit)
- Per-file repo/base_commit instead of top-level, eliminating multi-repo dispatch in CLI

## New Types

### CheckpointRequest

Replaces the current `CheckpointRequest`, `CheckpointRunRequest`, `LiveCheckpointRunRequest`, and `CapturedCheckpointRunRequest`.

```rust
pub struct CheckpointRequest {
    pub trace_id: String,
    pub checkpoint_kind: CheckpointKind,
    pub agent_id: Option<AgentId>,
    pub files: Vec<CheckpointFile>,
    pub path_role: PreparedPathRole,
    pub transcript_source: Option<TranscriptSource>,
    pub metadata: HashMap<String, String>,
}

pub struct CheckpointFile {
    pub path: PathBuf,           // absolute, always
    pub content: String,         // file content at snapshot time
    pub repo_work_dir: PathBuf,  // repo root this file belongs to
    pub base_commit_sha: String, // HEAD at snapshot time
}
```

Removed fields: `repo_working_dir` (top-level), `dirty_files`, `captured_checkpoint_id`, `file_paths`.

### Control API

```rust
ControlRequest::CheckpointRun {
    request: Box<CheckpointRequest>,
}
```

No `wait` field. No `Live`/`Captured` enum. `CheckpointRequest` goes directly over the socket.

### Bash Invocation Messages

```rust
ControlRequest::BashBeginInvocation {
    repo_working_dir: String,
    invocation_id: String,         // "{session_id}:{tool_use_id}"
    agent_context: InflightBashAgentContext,
    stat_snapshot: StatSnapshot,
}

ControlRequest::BashCompleteInvocation {
    repo_working_dir: String,
    invocation_id: String,
}
// Response returns stored StatSnapshot + agent_context, consuming the entry
```

`SnapshotWatermarks` stays as a separate message for the pre-hook watermark query (called before the snapshot is taken).

## CLI Side: handle_checkpoint

The entire function becomes:

1. Parse args: detect preset name + hook input
2. `synthesize_hook_input_from_cli_args` converts relative paths to absolute using CWD (mock presets only)
3. Call `execute_preset_checkpoint()` → `Vec<CheckpointRequest>`
4. Validate: every `CheckpointFile.path` is absolute (hard error if not)
5. Send each `CheckpointRequest` to daemon via control socket
6. If daemon send fails → log error, exit non-zero (no sync fallback)

Everything else currently in `handle_checkpoint` is deleted:
- Multi-repo detection / `group_files_by_repository`
- `checkpoint_context_from_active_bash` CLI-side fallback
- Bare Human checkpoint construction for no-preset calls
- Cross-repo dispatch loop
- `run_checkpoint_via_daemon_or_local`
- Captured checkpoint ceremony
- AgentUsage metric emission (moves to daemon)

## Orchestrator Changes

The orchestrator structure stays the same (parse hook input, route events, produce requests). Each `execute_*` handler now:

1. Resolves repo from file paths (unchanged — `find_repository_for_file`)
2. Runs `git rev-parse HEAD` → `base_commit_sha`
3. Reads file content from disk (or uses preset-provided content)
4. Builds `Vec<CheckpointFile>` with `{path, content, repo_work_dir, base_commit_sha}`

Files from different repos can coexist in a single `CheckpointRequest` since each `CheckpointFile` carries its own `repo_work_dir` and `base_commit_sha`. The daemon groups files by `repo_work_dir` when processing (it already has the family/repo concept). The orchestrator just resolves the per-file metadata and doesn't need to split requests.

`BashPreHookStrategy` is deleted. The pre-hook always does the full flow: store snapshot, find stale files, emit Human checkpoint if stale files exist. No-op when there are no stale files.

### Bash Pre-Hook Flow

1. Query `SnapshotWatermarks` from daemon → get watermarks
2. Take stat snapshot locally (core walk logic unchanged)
3. Send `BashBeginInvocation` to store snapshot + agent context in daemon memory
4. Find stale files using watermarks, read content
5. Return scoped `CheckpointRequest` with stale files (or None if none)

### Bash Post-Hook Flow

1. Send `BashCompleteInvocation` → get stored snapshot + agent context (consumed)
2. Take new stat snapshot locally
3. Diff pre vs post (unchanged logic)
4. Read changed file content, resolve repos, get base_commit_sha
5. Return scoped `CheckpointRequest` with changed files

## Daemon-Side Processing

### Checkpoint Reception

Daemon receives `CheckpointRequest` directly. It:

1. Groups `CheckpointFile` entries by `repo_work_dir`
2. Resolves git author identity per repo (moved from CLI to daemon)
3. Enqueues into `FamilySequencer` by repo (unchanged ordering guarantees)
4. Processes checkpoint: diffing, attribution, working log writes

### checkpoint.rs Adaptation

- `run()` signature simplifies — receives `CheckpointRequest` directly
- File content comes from `CheckpointFile.content` — no disk reads, no `dirty_files` resolution
- The two resolution paths (`resolve_base_override_dirty_file_execution` / `resolve_base_override_file_execution`) collapse into one since content is always provided
- All captured checkpoint functions deleted

### Bash Invocation Store

- `FamilyState` gets: `bash_invocations: HashMap<String, BashInvocation>`
- `BashInvocation` holds `{ agent_context, stat_snapshot, stored_at: Instant }`
- `BashBeginInvocation` → stores into map
- `BashCompleteInvocation` → removes and returns from map
- Daemon evicts entries older than 300s lazily or on a timer

### Pre-Commit via Daemon

`sync_pre_commit_checkpoint_for_daemon_commit` queries the in-memory `bash_invocations` on the family (instead of scanning `bash-snapshots/` on filesystem) to detect active AI context. Already produces scoped file lists from the committed diff — adapts to new `CheckpointRequest` shape.

### Metrics

- `AgentUsage` metric emission moves from CLI-side `handle_checkpoint` to daemon after checkpoint processing
- Per-file checkpoint metrics stay where they are (already daemon-side)

## Deletions

### Types

- `CheckpointRunRequest` enum, `LiveCheckpointRunRequest`, `CapturedCheckpointRunRequest`
- `PreparedCheckpointManifest`, `PreparedCheckpointFile`, `PreparedCheckpointCapture`
- `CapturedCheckpointInfo`, `BashPreHookStrategy`, `ActiveBashSnapshotScan`
- `CheckpointDispatchOutcome`

### Functions (git_ai_handlers.rs)

- `run_checkpoint_via_daemon_or_local`
- `checkpoint_request_has_explicit_capture_scope`
- `cleanup_captured_checkpoint_after_delegate_failure`
- `log_daemon_checkpoint_delegate_failure`
- `estimate_checkpoint_file_count`
- `get_all_files_for_mock_ai`

### Functions (checkpoint.rs)

- `prepare_captured_checkpoint`, `execute_captured_checkpoint`
- `delete_captured_checkpoint`, `load_captured_checkpoint_manifest`
- `explicit_capture_target_paths`
- `resolve_base_override_dirty_file_execution` (collapses into single path)
- `prune_stale_captured_checkpoints`

### Functions (bash_tool.rs)

- `save_snapshot`, `load_and_consume_snapshot`, `cleanup_stale_snapshots`
- `snapshot_cache_dir`, `sanitize_key`, `cache_entry_is_fresh`
- `attempt_pre_hook_capture`, `attempt_post_hook_capture`
- `scan_active_bash_snapshots`, `checkpoint_context_from_active_bash`

### Modules

- `authorship::pre_commit` (dead code, zero callers)

### Filesystem Artifacts Eliminated

- `~/.git-ai/internal/async-checkpoint-blobs/`
- `~/.git-ai/internal/bash-snapshots/`

### Control API Removed

- `CheckpointRunRequest::Captured` variant
- `wait` field on `CheckpointRun`
- `LiveCheckpointRunRequest` type

## What Stays Unchanged

- Preset parsing logic (each agent preset's `parse()` method)
- Bash stat snapshot core: `snapshot()`, `diff()`, watermark tiering, `find_stale_files`, ignore filtering
- Working log format and storage (`.git/ai/working-logs/{sha}.jsonl`)
- Attribution engine (`AttributionTracker::update_attributions_for_checkpoint`)
- Post-commit hook (authorship note generation)
- Rewrite tracking (`rebase_authorship.rs`)
- `FamilySequencer` ordering guarantees
- Git notes storage (`refs/notes/ai`)
- All other git hook handlers (rebase, cherry-pick, reset, stash, merge, etc.)

## What Changes Minimally

- Orchestrator structure (same event routing, handlers produce new type)
- `checkpoint.rs` core processing (same diffing/attribution, receives content directly)
- `daemon.rs` checkpoint enqueue/drain (new request shape, bash invocation store)
- `sync_pre_commit_checkpoint_for_daemon_commit` (queries daemon memory)
- Presets (output type changes, `BashPreHookStrategy` field removed from `PreBashCall`)
