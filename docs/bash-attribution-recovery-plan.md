# Bash Checkpoint Persistence And Attribution Recovery Plan

## Problem

Bash/shell tool attribution currently depends on a fast pre/post stat diff. That path is intentionally strict: the checkpoint process must stay cheap, the daemon only keeps pre-snapshots in memory, and the post hook only emits an AI checkpoint when it can identify changed paths immediately. Real usage shows that many shell-created or shell-modified lines still reach commit finalization as unknown/untracked, especially across repository roots, worktrees, daemon restarts, low-resolution mtimes, and command shapes that defeat the stat diff.

The goal is to keep the fast checkpoint path intact, but add daemon-owned durable evidence and post-commit recovery solvers that can convert otherwise unknown committed lines into AI session attributions when there is strong evidence.

## Current System Shape

- Agent presets parse hook JSON into `PreBashCall` / `PostBashCall`.
- `execute_pre_bash_call` calls `handle_bash_pre_tool_use_with_context`, which snapshots file stat tuples and sends `bash_session.start` to the daemon.
- The daemon stores active bash sessions only in `BashSessionState`, keyed by `(session_id, tool_use_id)`.
- `execute_post_bash_call` asks the daemon for the pre-snapshot, takes a post-snapshot, emits an `AiAgent` checkpoint only for changed paths, and then sends `bash_session.end`.
- Post-commit finalization builds a `VirtualAttributions`, splits line attributions into committed and uncommitted buckets, serializes an `AuthorshipLog`, writes `refs/notes/ai`, and records commit metrics.
- Unknown committed lines are simply lines absent from the note. Legacy `human` checkpoint attributions are deliberately stripped and become unknown/no-data.

## Design Principles

- Keep checkpoint subprocess overhead limited to parsing already-available hook fields and sending them over the existing control socket.
- Put durable bash history in the daemon, not in checkpoint command processes.
- Recover only unknown committed lines; never overwrite explicit AI or known-human attestations.
- Prefer minimizing unknown/untracked committed lines over preserving legacy `human` pre-bash holes during bash recovery.
- Add recovery after the normal authorship log is built and before custom attributes, note serialization, and stats. Existing session pruning happens inside the normal builder, so recovery must add any recovered session records itself.
- Model recovery as ordered solvers so future recovery stages can be added without changing post-commit control flow.
- Emit checkpoint metrics for recovered attribution with enough metadata to explain why recovery selected a session.

## Data Model

Add `src/daemon/bash_history_db.rs`, a dedicated SQLite database at:

`~/.git-ai/internal/bash-checkpoints-db`

Test override:

`GIT_AI_TEST_BASH_CHECKPOINT_DB_PATH`

Schema version 1:

```sql
CREATE TABLE bash_checkpoint_calls (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    invocation_key TEXT NOT NULL,
    repo_work_dir TEXT NOT NULL,
    session_id TEXT NOT NULL,
    tool_use_id TEXT NOT NULL,
    agent_tool TEXT NOT NULL,
    agent_external_id TEXT NOT NULL,
    agent_model TEXT NOT NULL,
    start_trace_id TEXT,
    end_trace_id TEXT,
    start_time_ns INTEGER NOT NULL,
    end_time_ns INTEGER,
    command TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX idx_bash_calls_repo_time
    ON bash_checkpoint_calls(repo_work_dir, start_time_ns, end_time_ns);

CREATE UNIQUE INDEX idx_bash_calls_invocation
    ON bash_checkpoint_calls(session_id, tool_use_id, start_trace_id);

CREATE INDEX idx_bash_calls_time
    ON bash_checkpoint_calls(start_time_ns, end_time_ns);
```

Retention:

- 30 days by `updated_at`.
- Prune at most once per day using `schema_metadata.bash_history_last_prune_ts`.

Rust record shape:

- `BashCheckpointCall`: persisted row with optional `end_time_ns`, optional command, and parsed metadata.
- `repo_work_dir` is retained for audit/debug metadata, but recovery candidate lookup is global by timestamp so cross-repo/cross-worktree bash commands can recover attribution.

## Control API Changes

No backward compatibility is required for the daemon control socket.

Extend `ControlRequest::BashSessionStart` with:

- `trace_id: String`
- `started_at_ns: u128`
- `command: Option<String>`

Extend `ControlRequest::BashSessionEnd` with:

- `repo_work_dir: String`
- `trace_id: String`
- `ended_at_ns: u128`
- `command: Option<String>`
- `agent_id: AgentId`
- `metadata: HashMap<String, String>`

The daemon still uses `BashSessionState` for active pre-snapshots, but also writes:

- Start row on `bash_session.start`.
- End update on `bash_session.end`.

If start was missed, end upserts a row with identical start/end timing as a best-effort record.

## Command Extraction

Add `parse::bash_command_from_hook_input(&serde_json::Value) -> Option<String>`.

Extraction order:

1. `tool_input.command`
2. `toolInput.command`
3. `tool_input.cmd`
4. `toolInput.cmd`
5. `command`
6. `cmd`
7. Agent-specific wrappers already parsed by each preset can pass through the same helper.

Store the result on both `PreBashCall` and `PostBashCall` as `command: Option<String>`. Preserve optionality because not every agent exposes command text.

## Recovery Pipeline

Add `src/authorship/attribution_recovery.rs`.

Entry point:

```rust
pub(crate) fn recover_attribution(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    human_author: &str,
    authorship_log: &mut AuthorshipLog,
    committed_hunks: &HashMap<String, Vec<LineRange>>,
) -> Result<(), GitAiError>
```

Post-commit flow calls this after `to_authorship_log_and_initial_working_log*` and background-agent filling, before custom attributes and note serialization. Because the normal builder has already pruned unused sessions, each solver that adds a new session-prefixed attestation must also insert the matching `SessionRecord`.

The committed hunk map must cover all changed files, not only checkpoint pathspecs. Recovery cannot find unknown files if post-commit only asks diff logic about AI-relevant pathspecs. Use the precomputed parent diff when available; otherwise collect parent-to-commit hunks without pathspec filtering for recovery.

Unknown line calculation:

- Expand committed hunk lines per file.
- Expand existing authorship log entries per file.
- Unknown lines are committed hunk lines not covered by any existing attestation entry.
- A file is eligible when it has at least one unknown line.

## Solver 1: Bash Mtime/Ctime Recovery

For each eligible file:

1. Read the committed file metadata from the working tree when the committed file matches the working tree.
2. Use both `mtime` and `ctime` when available, converted to nanoseconds since epoch.
3. Query global bash history for calls whose `[start_time_ns, end_time_ns]` window is within ±3 seconds of either file timestamp.
4. Score candidates by nearest timestamp distance, then prefer completed calls, then prefer calls with command text, then newest row id.
5. If a candidate is selected, add an attestation for all unknown committed lines in that file to:

`generate_session_id(agent_external_id, agent_tool)::generate_trace_id()`

Also ensure `authorship_log.metadata.sessions` contains the recovered session record.

Recovery metadata metric JSON should include:

- `solver`: `"bash_mtime"`
- `file_path`
- `unknown_lines`
- `target_repo_work_dir`
- `file_timestamps_ns`
- `selected_bash_call_id`
- `selected_bash_repo_work_dir`
- `selected_tool_use_id`
- `selected_command`
- `distance_ns`
- `window_ns`
- `start_time_ns`
- `end_time_ns`
- `start_trace_id`
- `end_trace_id`

Metric:

- Event ID: existing checkpoint event.
- `kind`: `"ai_agent"`
- `edit_kind`: `"bash"`
- `checkpoint_type`: `"recovered_bash"` as a new value field.
- `attribution_recovery_metadata`: JSON string as a new value field.
- Timestamp: original bash start timestamp when available.
- Attributes: repo URL, branch, session id, trace id, tool, model, external session id, base/commit sha.

## Solver 2: Adjacent Edge Bridge

Run after bash recovery.

For each remaining eligible file:

1. Build a map of line -> author from current authorship log entries.
2. Identify contiguous unknown line runs inside committed hunks.
3. Recover attribution when the unknown run is directly between two AI-attributed neighbors from the same session key.
4. Recover up to three leading or trailing unknown lines when a run touches one AI-attributed edge and is open-ended on the other side.
5. Do not extend from known-human (`h_`) or legacy `human`.
6. Do not bridge between two different AI sessions.
7. Add a new trace id while preserving the recovered or original session id prefix.
8. Only recover lines from the remaining unknown set derived from committed hunks, so earlier solvers are not overridden.

Metric:

- Existing checkpoint event.
- `kind`: `"ai_agent"`
- `edit_kind`: `"attribution_recovery_edge"`
- `checkpoint_type`: `"recovered_edge_extension"`
- `attribution_recovery_metadata`: JSON with solver, file, source author, source neighboring lines, recovered ranges, and reason.

## Metric Schema Changes

Extend checkpoint event positions:

- `9`: `checkpoint_type` string nullable.
- `10`: `attribution_recovery_metadata` string nullable.

Keep existing positions stable.

## Tests First

Add RED tests before implementation:

1. Bash DB unit tests:
   - start/end lifecycle persists all fields.
   - end without start upserts best-effort row.
   - query returns candidates inside ±3 seconds and excludes outside.
   - retention prunes rows older than 30 days.

2. Preset/orchestrator unit tests:
   - Codex, Claude, Gemini, Cursor, Windsurf, Copilot CLI/IDE command extraction where payloads expose command.
   - Missing command remains `None`.

3. Integration: bash recovery:
   - Codex pre/post bash hook records durable bash history.
   - The file is modified by the simulated bash command.
   - No AI checkpoint is emitted for the file, so normal attribution would be unknown.
   - Commit recovery attributes unknown committed lines to the recovered AI session.
   - Assert committed blame and authorship note session metadata.

4. Integration: closest candidate:
   - Two bash calls around the file timestamp.
   - Recovery selects the closest one.

5. Integration: outside window:
   - Bash call outside ±3 seconds leaves unknown lines unknown.

6. Integration: edge extension:
   - AI checkpoints produce matching AI-attributed lines around an unknown gap.
   - Commit recovery bridges only the unknown gap between the matching AI session lines.
   - Unknown leading and trailing lines next to an AI block recover up to three lines per side.

7. Integration: edge extension guardrails:
   - Unknown line between two different AI sessions remains unknown.
   - Unknown line adjacent only to known-human remains unknown.
   - Unknown line with one AI neighbor and a known-human or different-session neighbor remains unknown.

8. Metric unit tests:
   - checkpoint sparse encoding includes the new nullable fields.
   - recovery metric builders emit expected `checkpoint_type` and JSON metadata.

After RED tests fail for the expected reasons, implement the smallest complete slice, then expand until all scenarios pass.

## Implementation Order

1. Add and test `BashHistoryDatabase`.
2. Extend control API and daemon handling to write bash history.
3. Add command extraction and thread command strings through bash events.
4. Add committed-hunk helpers for recovery without disturbing the existing pathspec-limited normal attribution pass.
5. Add recovery framework and bash mtime solver.
6. Add checkpoint metric schema extensions and recovery metric emission.
7. Add edge-extension solver.
8. Update affected existing assertions where edge extension legitimately changes expected attribution.
9. Run focused tests, `task fmt`, `task lint`, and broader suites as time allows.

## Review Pass Notes

- The durable DB is daemon-owned and use-case-specific, matching the notes/metrics DB pattern instead of extending the deprecated internal DB.
- Recovery never rewrites existing attestation entries; it only covers holes.
- Solver ordering matters: bash evidence gets first claim over unknown lines, and edge bridging only sees the remaining holes.
- Session records are required because recovered entries use session-prefixed hashes.
- Full committed hunk coverage is required for recovery; pathspec-limited hunk maps are insufficient.
- Metrics must be emitted at recovery time because no working-log checkpoint exists for recovered lines.
