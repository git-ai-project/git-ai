# Checkpoint Rewrite Design

First-principles rewrite of the checkpoint system to minimize CLI latency, eliminate unscoped checkpoints, and consolidate all processing in the daemon.

## Core Principles

1. The checkpoint CLI subcommand is latency-critical. It does the absolute minimum: gather file contents, resolve repos, send to daemon.
2. All processing (diffing, attribution, metrics, author resolution) happens in the daemon.
3. No unscoped checkpoints. Every checkpoint is scoped (explicit file list) or bash (watermark-based, which discovers its own files).
4. All file paths are absolute. No relative path resolution anywhere except the mock preset CLI args helper.
5. No local/synchronous fallback. If the daemon is unreachable, hard fail.
6. No disk-based intermediate state for checkpoint data. File contents flow over the control socket. Bash pre-snapshots live in daemon memory.

## New Types

### CheckpointRequest

The single type that flows from CLI to daemon:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRequest {
    pub trace_id: String,
    pub checkpoint_kind: CheckpointKind,
    pub agent_id: Option<AgentId>,
    pub files: Vec<CheckpointFile>,
    pub path_role: PreparedPathRole,
    pub transcript_source: Option<TranscriptSource>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFile {
    pub path: PathBuf,              // absolute path to file
    pub content: Option<String>,    // file content at snapshot time (None = deleted/missing)
    pub repo_work_dir: PathBuf,     // repo root for this file
    pub base_commit: BaseCommit,    // HEAD of repo at snapshot time
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BaseCommit {
    Sha(String),
    Initial,  // no commits yet (brand new repo)
}
```

### Control API

```rust
enum ControlRequest {
    CheckpointRun {
        request: Box<CheckpointRequest>,
    },
    BashSessionStart {
        repo_work_dir: String,
        session_id: String,
        tool_use_id: String,
        agent_id: AgentId,
        metadata: HashMap<String, String>,
        stat_snapshot: StatSnapshot,
    },
    BashSessionEnd {
        session_id: String,
        tool_use_id: String,
    },
    BashSessionQuery {
        repo_work_dir: String,
    },
    BashSnapshotQuery {
        session_id: String,
        tool_use_id: String,
    },
    // ... existing non-checkpoint variants unchanged
}
```

### Daemon Bash State

```rust
struct BashSessionState {
    sessions: HashMap<(String, String), BashSession>,  // (session_id, tool_use_id)
}

struct BashSession {
    repo_work_dir: String,
    agent_id: AgentId,
    metadata: HashMap<String, String>,
    stat_snapshot: StatSnapshot,
    started_at: Instant,
}
```

### Removed Types

- `CheckpointRunRequest` enum (Live/Captured)
- `LiveCheckpointRunRequest`
- `CapturedCheckpointRunRequest`
- `PreparedCheckpointManifest`
- `DirtyFileContent`
- `dirty_files` field (everywhere)
- `captured_checkpoint_id` field (everywhere)
- `wait` field on `CheckpointRun`
- `BashPreHookStrategy` enum (all agents now use explicit pre/post file edit events)
- `is_pre_commit` parameter threaded through checkpoint.rs — with `pre_commit.rs` dead and the daemon never setting this to true, all branches guarded by `is_pre_commit` are dead code

## Component Designs

### handle_checkpoint (CLI subcommand)

Rewritten to ~50 lines:

1. Parse args: extract preset name, `--hook-input` (or stdin), remaining args.
2. Run preset: `execute_preset_checkpoint(preset_name, hook_input)` returns `Vec<CheckpointRequest>`.
3. Validate: every `file.path` must be absolute. Error and exit if not.
4. Ensure daemon: get daemon socket, hard fail if unavailable.
5. Send each request over control socket, hard fail on timeout/error.
6. Exit.

No repo discovery, no multi-repo grouping, no author identity, no metrics, no captured checkpoint logic, no relative path resolution. The multi-repo case is handled naturally since each `CheckpointFile` carries its own `repo_work_dir`.

`synthesize_hook_input_from_cli_args` gains one change: for mock presets, relative paths get resolved to absolute using `cwd` before returning the JSON. This is the only place relative-to-absolute conversion happens.

### Orchestrator

The `execute_*` functions change to produce the new types:

- Resolve `repo_work_dir` per file via `discover_repository_in_path_no_git_exec` (walks up tree for `.git`, no git exec).
- Get `base_commit` per repo via `rev-parse HEAD` (one call per unique repo, cached across files sharing a repo).
- Read file content from disk at snapshot time.
- Assemble `CheckpointFile` structs.

For bash events:
- `execute_pre_bash_call`: sends `BashSessionStart` to daemon (stat snapshot + agent context). No checkpoint emission — all agents now send explicit pre/post file edit events, so the baseline is always established through those scoped checkpoints. `BashPreHookStrategy` is removed entirely.
- `execute_post_bash_call`: sends `BashSnapshotQuery` to daemon to retrieve pre-snapshot, stats filesystem, diffs pre vs post, reads changed file contents, packs into `CheckpointFile` structs, signals `BashSessionEnd`.

### Daemon

**Checkpoint ingestion**: receives `CheckpointRequest`, groups files by `repo_work_dir`, resolves each to a family, appends to family sequencer.

**Checkpoint processing** (`apply_checkpoint_side_effect`): no Live/Captured branching. One code path. Resolves git author identity here (not in CLI). Produces MetricsEvent here (not in CLI).

**Bash session state**: in-memory `HashMap<(session_id, tool_use_id), BashSession>`. Handles `BashSessionStart`, `BashSessionEnd`, `BashSessionQuery`, `BashSnapshotQuery` control requests.

### Pre-commit

`src/authorship/pre_commit.rs` (`pre_commit()` and `pre_commit_checkpoint_context()`) is dead code — no caller exists. The real pre-commit checkpoint logic lives in `sync_pre_commit_checkpoint_for_daemon_commit()` inside `daemon.rs`. Delete the dead module. The daemon-side function will be updated to work with the new types as part of the daemon rewrite (Layer 3).

## What Gets Deleted

### Functions in git_ai_handlers.rs
- `run_checkpoint_via_daemon_or_local` (~240 lines)
- `checkpoint_request_has_explicit_capture_scope`
- `get_all_files_for_mock_ai`
- `group_files_by_repository` (checkpoint usage)
- `estimate_checkpoint_file_count`
- `log_daemon_checkpoint_delegate_failure`
- `cleanup_captured_checkpoint_after_delegate_failure`
- `log_performance_for_checkpoint` (moves to daemon)

### Functions in checkpoint.rs
- `prepare_captured_checkpoint`
- `execute_captured_checkpoint`
- `update_captured_checkpoint_agent_context`
- `explicit_capture_target_paths`
- `cleanup_failed_captured_checkpoint_prepare`
- The local/synchronous `run()` entrypoint

### Bash tool
- `checkpoint_context_from_active_bash`
- `scan_active_bash_snapshots`
- Disk-based snapshot file I/O (write/read/cleanup of `bash-snapshots/*.json`)
- `InflightBashAgentContext` serialization

### Pre-commit (dead code — delete entire module)
- `src/authorship/pre_commit.rs` (`pre_commit()`, `pre_commit_checkpoint_context()`)

### Filesystem artifacts
- `~/.git-ai/internal/async-checkpoint-blobs/` directory and all blob management
- `bash-snapshots/*.json` files

## What Stays Unchanged

- **Bash watermark system**: `DaemonWatermarks`, tier 1/2/3 coverage logic, mtime grace window, stat-based change detection algorithm. Storage medium for pre-snapshots moves from disk to daemon memory, but the logic stays.
- **Working log format**: `.git/ai/working_logs/<base_commit>/` structure, JSONL checkpoints, blob storage, `PersistedWorkingLog`.
- **Authorship notes**: post-commit hook reading working logs, generating `AuthorshipLog`, git notes under `refs/notes/ai`.
- **Rewrite tracking**: rebase/cherry-pick/reset hooks and `rebase_authorship.rs`.
- **Agent presets**: preset trait, `parse()` implementations, `ParsedHookEvent` variants. Minimal changes.
- **Family sequencer / family actor**: internal daemon processing pipeline. Input type changes but sequencing model stays.
- **Config, feature flags, error types**.

## Implementation Order

1. **Types**: define new `CheckpointRequest`, `CheckpointFile`, `BaseCommit`, new control API types, bash session types. Old types coexist temporarily.
2. **CLI**: rewrite `handle_checkpoint` to ~50 lines. Update orchestrator `execute_*` functions. Update `synthesize_hook_input_from_cli_args`.
3. **Daemon**: update control API handler. Implement bash session in-memory state + 4 new handlers. Update `ingest_checkpoint_payload` and `apply_checkpoint_side_effect`. Move author identity and metrics here.
4. **Dead code removal**: delete `src/authorship/pre_commit.rs` and its module declaration. Update `sync_pre_commit_checkpoint_for_daemon_commit()` in daemon.rs to work with new types.
5. **Bash tool**: remove disk-based snapshot I/O. Pre-hook sends stat snapshot to daemon. Post-hook queries daemon for pre-snapshot, diffs, reads changed files. Remove `BashPreHookStrategy`.
6. **Delete dead code**: all functions, types, and filesystem artifacts from the deletion list.
7. **Fix tests**: update integration tests. Evaluate bash test failures for correctness vs band-aid issues around mtime grace periods.
