# Checkpoint Logic Rewrite

First-principles rewrite of the checkpoint ingestion pipeline: subcommand, control API, and daemon processing.

## Goals

- Reduce `handle_checkpoint` from ~580 lines to ~40
- Eliminate unscoped checkpoints, captured checkpoint ceremony, sync fallback
- Move all processing (diffing, attribution, metrics, author resolution) to the daemon
- Pass file contents directly over the control socket (no temp files on disk)
- Absolute file paths only — enforced at ingestion boundary
- Per-file repo and base_commit — no top-level repo_working_dir
- One request type, one processing path for all checkpoint sources

## New Types

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRequest {
    pub trace_id: String,
    pub checkpoint_kind: CheckpointKind,
    pub agent_id: Option<AgentId>,
    pub files: Vec<CheckpointFileEntry>,
    pub path_role: PreparedPathRole,
    pub transcript_source: Option<TranscriptSource>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFileEntry {
    pub path: PathBuf,           // absolute path to file
    pub content: String,         // file content at snapshot time
    pub repo_work_dir: PathBuf,  // git repo root for this file
    pub base_commit_sha: String, // HEAD sha at snapshot time; empty string = no prior commit (initial)
}
```

For deleted files, `content` is an empty string. The daemon handles this the same way it handles deletions today.

### Removed from CheckpointRequest

- `repo_working_dir` (top-level) — now per-file in `CheckpointFileEntry`
- `file_paths` — replaced by `files` vec
- `dirty_files` — content is always in `files[].content`
- `captured_checkpoint_id` — no more captured checkpoint ceremony

## Control API

```rust
#[serde(rename = "checkpoint.run")]
CheckpointRun {
    request: Box<CheckpointRequest>,
},
```

- No `wait` parameter — always async (fire and forget from subcommand)
- No `CheckpointRunRequest` enum — `CheckpointRequest` goes directly on the wire
- No `LiveCheckpointRunRequest` / `CapturedCheckpointRunRequest` wrappers

## Checkpoint Subcommand (~40 lines)

1. Parse args: preset name, hook input (--hook-input or stdin)
2. `execute_preset_checkpoint(preset_name, hook_input)` returns `Vec<CheckpointRequest>` with fully populated `files` vec
3. Send each request over control socket to daemon. On failure: log and exit (no sync fallback)
4. Emit `AgentUsage` metric if AI checkpoint (stays client-side, per-invocation)

### Eliminated from subcommand

- All repo detection (find_repository_in_path, group_files_by_repository, multi-repo workspace logic)
- run_checkpoint_via_daemon_or_local and its entire cascade
- checkpoint_context_from_active_bash (was for unscoped pre-commit)
- get_all_files_for_mock_ai (unscoped support)
- Author identity resolution (moved to daemon)
- Captured checkpoint prepare/submit/cleanup
- Cross-repo external file handling

## Orchestrator Changes

Each event handler in the orchestrator populates `files` vec directly:
- Resolve repo per file using existing `.git` walk-up helpers (no git call)
- Get base_commit_sha via `git rev-parse HEAD` per repo (one git call per distinct repo)
- Read file content from disk
- Pack into `CheckpointFileEntry`

`synthesize_hook_input_from_cli_args` for mock presets converts relative paths to absolute using CWD before anything else. All downstream code sees only absolute paths. Non-absolute paths after preset parsing are a hard error.

### Bash flow

- **Pre-bash**: Stat scan to find already-dirty files. Read their contents. Resolve repo/base_commit per file. Emit Human checkpoint with populated `files` vec. Also takes stat snapshot (unchanged).
- **Post-bash**: Stat diff pre/post snapshots to find changed files. Read their contents. Resolve repo/base_commit per file. Emit AI checkpoint with populated `files` vec. Stat snapshot consumed/deleted (unchanged).
- **Watermark logic**: Completely unchanged. Only the output packaging changes.

## Daemon Processing

### handle_control_request

1. Receives `CheckpointRun`, extracts `CheckpointRequest`
2. Groups `request.files` by `repo_work_dir`
3. For each repo group: resolve family, append to family sequencer
4. Returns `ControlResponse::ok` immediately

### apply_checkpoint_side_effect

1. Resolve git author identity from repo (moved here from subcommand)
2. Process checkpoint: diff, attribution, working log updates. File content from `files[].content`. Previous state from working log or `git show base_commit_sha:path`.
3. Record `MetricsEvent` per file (moved here from subcommand)
4. Notify transcript worker if `transcript_source` present

### Daemon commit replay (sync_pre_commit_checkpoint_for_daemon_commit)

Constructs a `CheckpointRequest` with the new type:
- File content from `committed_file_snapshot_between_commits`
- base_commit_sha and repo_work_dir already known
- Feeds into same processing path as all checkpoints
- Pre-commit optimizations (skip human-only files, early bail if no AI edits) become pre-filtering before constructing the request
- `base_commit_override` and `BaseOverrideResolutionPolicy` eliminated — base_commit is per-file

## Dead Code Removal

### Types removed
- `CheckpointRunRequest` enum (Live/Captured)
- `LiveCheckpointRunRequest`
- `CapturedCheckpointRunRequest`
- `PreparedCheckpointManifest`
- `BaseOverrideResolutionPolicy` enum
- `CheckpointDispatchOutcome`

### Functions removed from checkpoint.rs
- `prepare_captured_checkpoint`
- `execute_captured_checkpoint`
- `delete_captured_checkpoint`
- `update_captured_checkpoint_agent_context`
- `explicit_capture_target_paths`
- `resolve_implicit_path_execution`
- All `is_pre_commit` parameter threading

### Functions removed from git_ai_handlers.rs
- `run_checkpoint_via_daemon_or_local`
- `checkpoint_request_has_explicit_capture_scope`
- `cleanup_captured_checkpoint_after_delegate_failure`
- `log_daemon_checkpoint_delegate_failure`
- `get_all_files_for_mock_ai`
- `estimate_checkpoint_file_count`
- All multi-repo workspace detection/grouping in handle_checkpoint

### Files removed
- `authorship/pre_commit.rs` (dead code)

### Daemon cleanup
- `Captured` match arm in `apply_checkpoint_side_effect`
- `wait` logic in `ingest_checkpoint_payload`
- `is_pre_commit` from control API types

### Working log cleanup
- `dirty_files` field and `set_dirty_files`
- Fallback content reads from dirty_files

## Unchanged

- Bash stat snapshot/watermark system (StatEntry, StatSnapshot, watermark queries)
- Agent presets (resolve_preset, parse, ParsedHookEvent variants, individual preset implementations)
- Working log storage format (on-disk JSON, blob storage, line attributions)
- Authorship note generation (post-commit hook, AuthorshipLog schema, refs/notes/ai)
- Rewrite tracking (rebase/cherry-pick/reset/merge authorship rewriting)
- Family sequencer (per-family queueing and ordering)
- Transcript worker (session/transcript handling)
- Config, feature flags, signal forwarding, git proxy dispatch
