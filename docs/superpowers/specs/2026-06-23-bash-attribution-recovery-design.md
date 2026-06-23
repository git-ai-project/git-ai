# Bash Attribution Recovery — Design

**Date:** 2026-06-23
**Branch:** `feat/expand-bash-support-cc`
**Status:** Design (pre-implementation)

## Problem

Real-world git-ai usage shows that a large fraction of AI-agent *bash/shell* changes
land in commits as **untracked** (no AI or KnownHuman attribution). Bash checkpoints
are intentionally cheap and therefore fragile: they require strict timing guarantees,
a matching `cwd`, and a matching repo working dir between the pre- and post-tool-use
hooks. Cross-repo, cross-worktree, and slow/long-running shell commands routinely
violate these constraints, so the changes those commands produce show up as holes in
the authorship log.

We want to *recover* this attribution after the fact, during post-commit finalization,
without making the checkpoint hot path any slower.

## Goals

1. **Persist bash checkpoint facts** in a dedicated, daemon-managed SQLite DB with
   30-day retention. Store enough to later correlate a committed file's mtime/ctime
   with the shell command that most likely produced it.
2. **Recover attribution post-commit** via a pluggable pipeline of *solvers* that run
   after the initial authorship log is built but before the note is written, targeting
   only files that still have unknown/untracked lines:
   - **Solver 1 — Bash mtime/ctime correlation:** match untracked files to a bash
     checkpoint whose time window brackets the file's mtime/ctime (±3s), and cover the
     file's untracked lines to that bash session.
   - **Solver 2 — AI edge extension:** absorb untracked lines that sit directly
     adjacent to AI-attributed lines (the Myers-diff "edge drift" problem) into the
     neighboring AI session.
3. **Emit tracking metrics** for every recovery: a `recovered_bash` checkpoint metric
   event (kind `ai_agent`, edit_kind `bash`) carrying the original bash session's ts /
   session id, plus a new free-form **attribution recovery metadata** attribute (JSON)
   explaining why the solver fired.

## Non-Goals

- No changes to the checkpoint *spawn* hot path beyond passing already-available data
  over the existing control socket. All DB writes happen in the daemon.
- No backwards compatibility on the daemon control socket (daemon & CLI update
  atomically — established invariant).
- Solver 1 does not try to attribute to a *specific* file the bash command touched;
  bash commands rarely tell us which files they wrote. It correlates by time window
  and covers the whole file's untracked lines to the matched session.

---

## Background: how things work today (verified against source)

### Bash checkpoint flow
- AI presets classify a tool call as `ToolClass::Bash`
  (`src/commands/checkpoint_agent/bash_tool.rs:308`). On `PreToolUse` they emit
  `PreBashCall` and on `PostToolUse` `PostBashCall`
  (`presets/mod.rs:82-93`, `presets/claude.rs:101-124`).
- Correlation between pre/post is the pair **`(external_session_id, tool_use_id)`**.
  The daemon stores the pre-snapshot in-memory keyed by that pair
  (`src/daemon/bash_sessions.rs:18,40-56`).
- Pre-hook → `BashSessionStart` (stores `StatSnapshot` + agent_id + metadata in the
  daemon). Post-hook → `BashSnapshotQuery` then `BashSessionEnd`
  (`control_api.rs:29-49`, `daemon.rs:5065-5145`).
- The actual **command string is NOT captured today** for most agents (only Windsurf
  reads `tool_input.command`, `presets/windsurf.rs:196`). Claude only extracts
  `tool_use_id` for bash (`presets/claude.rs:63`).
- Timing today is `Instant`-based and in-memory only; nothing about a bash call is
  durably persisted.

### SQLite DB conventions
Four DBs exist, all following an identical pattern (`src/notes/db.rs` is the cleanest
reference): `OnceLock<Mutex<Db>>` singleton via `global()`, `open_at_path()` for tests,
`database_path()` honoring a `GIT_AI_TEST_*_DB_PATH` env override, WAL pragmas, and a
versioned `MIGRATIONS: &[&str]` array with a `schema_metadata(key,value)` version row.
Only the metrics DB has time-based retention/pruning
(`src/metrics/db.rs:655-693`, 365-day window, ≥24h between passes).

### Post-commit finalization
`post_commit_from_working_log_with_transform_options_and_diff`
(`src/authorship/post_commit.rs:144-410`) builds the `AuthorshipLog`, then:
1. Runs `background_agent::fill_unattributed_lines` for no-hooks cloud agents
   (`post_commit.rs:205-253`) — this is the existing "hole-filling" precedent.
2. Applies the `transform` hook (`post_commit.rs:255`).
3. Injects custom attributes, serializes, writes the note (`post_commit.rs:261-278`).

Lines are "unknown" by **absence** from any `AttestationEntry`
(`authorship_log_serialization.rs:62-73`; `stats.rs:22` computes `unknown_additions`).
`background_agent.rs:90-165` is the canonical example of computing committed-but-
unattributed line ranges and adding a `SessionRecord` + `AttestationEntry` whose hash is
`format!("{session_key}::{trace_id}")`.

### Metric events
`CheckpointValues` (`src/metrics/events.rs:443-485`) is position-encoded; adding a field
means: new `checkpoint_pos` const, new struct field, builder method, and
`to_sparse`/`from_sparse` wiring. `EventAttributes` already has `custom_attributes`
(`attrs.rs`, position 30) and `session_id`/`trace_id`. Metric emission is
`crate::metrics::record(values, attrs)` (`metrics/mod.rs:49`); dropped in tests.

---

## Architecture

### Component 1 — `bash_checkpoints` SQLite DB (`src/daemon/bash_checkpoints_db.rs`)

A new daemon-owned DB at `~/.git-ai/internal/bash-checkpoints-db`, modeled exactly on
`src/notes/db.rs`. Test override env: `GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH`.

**Schema (migration 0→1):**
```sql
CREATE TABLE IF NOT EXISTS bash_checkpoints (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id          TEXT NOT NULL,          -- external_session_id
    tool_use_id         TEXT NOT NULL,          -- correlation id (the "bash checkpoint id")
    repo_work_dir       TEXT NOT NULL,          -- worktree state key
    tool                TEXT NOT NULL,          -- agent_id.tool
    agent_model         TEXT,                   -- agent_id.model
    agent_internal_id   TEXT,                   -- agent_id.id
    command             TEXT,                   -- bash command string when available
    start_ns            INTEGER NOT NULL,       -- pre-hook wall-clock (UNIX ns)
    end_ns              INTEGER,                -- post-hook wall-clock (UNIX ns); NULL until ended
    created_at          INTEGER NOT NULL        -- UNIX seconds (for retention)
);
CREATE INDEX IF NOT EXISTS idx_bash_ckpt_repo_time
    ON bash_checkpoints(repo_work_dir, start_ns);
CREATE UNIQUE INDEX IF NOT EXISTS idx_bash_ckpt_corr
    ON bash_checkpoints(session_id, tool_use_id);
```

Rationale for fields: `tool_use_id` *is* the existing per-bash-call correlation id (the
"start/end trace id" referenced in the goal — there is no separate trace id for bash
hooks; the checkpoint `trace_id` is generated per checkpoint-run, not stored per bash
call). We persist wall-clock start/end in **nanoseconds** (UNIX epoch) so we can compare
against file mtime/ctime, which are also `SystemTime`. `command` is nullable because not
all agents expose it.

**API (mirrors `NotesDatabase`):**
- `global() -> &'static Mutex<BashCheckpointsDatabase>`
- `open_at_path(path) -> Self` (tests)
- `record_start(session_id, tool_use_id, repo_work_dir, agent_id, command, start_ns)` —
  upsert on `(session_id, tool_use_id)`.
- `record_end(session_id, tool_use_id, end_ns)` — set `end_ns` on the matching row.
- `find_candidates(repo_work_dir, window_lo_ns, window_hi_ns) -> Vec<BashCheckpointRow>`
  — rows for the repo whose `[start_ns, end_ns]` interval (treating NULL end as `start_ns`)
  overlaps the query window. Used by Solver 1.
- `prune_old(now_secs)` — `DELETE FROM bash_checkpoints WHERE created_at < now - 30d`,
  rate-limited to ≥24h between passes via a `schema_metadata` cursor key
  (same pattern as metrics pruning). Called from the daemon's existing periodic
  maintenance / on `record_start`.

**Retention:** 30 days (`BASH_CKPT_RETENTION_SECS = 30 * 86400`).

### Component 2 — Control socket plumbing

The daemon already receives `BashSessionStart`/`BashSessionEnd`. We persist from those
existing handlers — **no new control message types needed** for start/end. We extend the
two existing variants with the data the DB needs:

- `BashSessionStart` gains `command: Option<String>` and `start_ns: u128`.
- `BashSessionEnd` gains `end_ns: u128`.

(Wall-clock times are computed client-side at hook time; the daemon persists them.)

In `daemon.rs:5065` (`BashSessionStart` handler) and `:5084` (`BashSessionEnd`), after
updating the in-memory `BashSessionState`, write to `BashCheckpointsDatabase::global()`.
DB write failures are logged and ignored (never block the checkpoint).

The CLI side (`orchestrator.rs:397-488` and `bash_tool.rs` send paths) threads the
command string (extracted in the preset) and wall-clock timestamps into those control
messages. Command extraction: add `command: Option<String>` to `PreBashCall`/`PostBashCall`
context and populate it in each preset that exposes `tool_input.command`
(Claude, Cursor, Gemini, Codex, etc.); leave `None` where unavailable. To stay DRY, add a
shared `parse::bash_command_from_tool_input(&Value)` helper.

### Component 3 — Attribution recovery pipeline (`src/authorship/recovery/`)

New module tree:
```
src/authorship/recovery/
    mod.rs           -- AttributionRecovery orchestrator + RecoverySolver trait + shared types
    bash_solver.rs   -- Solver 1: bash mtime/ctime correlation
    edge_solver.rs   -- Solver 2: AI edge extension
```

**Trait:**
```rust
pub struct RecoveryContext<'a> {
    pub repo: &'a Repository,
    pub commit_sha: &'a str,
    pub parent_sha: &'a str,
    pub repo_work_dir: &'a Path,
    pub committed_hunks: &'a HashMap<String, Vec<LineRange>>, // added lines per file
    pub human_author: &'a str,
}

/// Result of a solver covering some unknown lines.
pub struct RecoveredAttribution {
    pub session_key: String,
    pub trace_id: String,
    pub session_record: SessionRecord,
    pub per_file_lines: HashMap<String, Vec<LineRange>>,
    pub metric: RecoveredCheckpointMetric, // drives the recovered_* metric event
}

pub trait RecoverySolver {
    fn name(&self) -> &'static str;
    fn solve(
        &self,
        ctx: &RecoveryContext,
        unknown: &UnknownLines, // per-file unknown line sets, recomputed from the log
    ) -> Vec<RecoveredAttribution>;
}
```

**Orchestrator `recover_attribution(authorship_log, ctx, solvers)`:**
1. Compute `UnknownLines` = committed_hunks minus already-attributed lines (reuse the
   exact logic in `background_agent.rs:103-134`, extracted into a shared
   `unknown_lines(authorship_log, committed_hunks)` helper so both call sites stay DRY).
2. For each solver in order: run it against the *current* unknown set, apply its
   `RecoveredAttribution`s to the log (insert `SessionRecord`, add `AttestationEntry`s),
   recompute the unknown set, and collect metrics. Later solvers see fewer unknowns.
3. Return the collected `RecoveredCheckpointMetric`s for emission.

This is wired into `post_commit.rs` immediately **after** `fill_unattributed_lines` and
**before** `transform` (`post_commit.rs:253`→`255`). It runs for all agents (not just
no-hooks). It reuses the already-computed `committed_hunks` (today only computed in the
no-hooks branch — we hoist that computation so it is always available when any solver is
enabled).

**Solver 1 — `BashCorrelationSolver`:**
- For each file with unknown lines, `lstat` the worktree file and read mtime & ctime
  (reuse `StatEntry::from_metadata`, `bash_tool.rs:154`).
- Query `BashCheckpointsDatabase::find_candidates(repo_work_dir, t-3s, t+3s)` for both
  mtime and ctime windows.
- Choose the **closest** candidate (min |file_time − nearest end_ns/start_ns|). Tie-break
  toward the most recent. If a match exists, produce a `RecoveredAttribution` covering
  **all** the file's unknown lines to that bash session.
  - session_key derived from the candidate's `(agent_internal_id, tool)` via
    `generate_session_id` (consistent with existing session hashing).
  - metric: kind `ai_agent`, edit_kind `bash`, ts = candidate end (or start), session id =
    candidate session, recovery metadata JSON = `{ solver: "bash_correlation",
    tool_use_id, command (truncated), file_mtime_ns, file_ctime_ns, matched_edge:
    "start|end", delta_ns }`.
- Skips files whose mtime/ctime are unavailable or outside any window.

**Solver 2 — `AiEdgeExtensionSolver`:**
- For each file, look at its existing AI attestations (entries whose session record has an
  AI `agent_id`; exclude human/known-human). Build the set of AI-attributed lines.
- An unknown line is "extendable" iff it is directly adjacent (line ±1, transitively
  within the unknown run) to an AI-attributed line **and** the run of unknown lines is
  bounded on at least one side by AI lines. Concretely: for each maximal run of unknown
  lines, if the line immediately above the run OR immediately below the run is
  AI-attributed, absorb the whole run into the nearest such AI session (prefer the
  session that owns the larger adjacent block; tie-break to the block above).
- Carry over the **same session id** as the adjacent AI block, but mint a **new trace id**
  so the recovery is distinguishable. metric: kind `ai_agent`, edit_kind = original
  block's edit_kind or `extension`, recovery metadata JSON = `{ solver:
  "ai_edge_extension", extended_from_session, adjacent_side: "above|below|both",
  run_lines }`.
- This solver intentionally only touches lines adjacent to *AI* code, never human code,
  so it can't steal attribution from KnownHuman.

### Component 4 — Recovered metric events

Add one new attribute carrying recovery metadata. Options considered:
- (A) Reuse `EventAttributes.custom_attributes` (position 30) — already JSON, already
  injected for prompts/sessions. **Risk:** collides with user-configured custom
  attributes that `post_commit.rs:261-272` injects.
- (B) **Add a dedicated `attribution_recovery_metadata` value field on
  `CheckpointValues`** (new `checkpoint_pos` = 9). **Chosen** — isolated, explicit,
  matches the position-encoded pattern, no collision.

Emission: a small `emit_recovered_metrics(repo, commit_sha, parent_sha, human_author,
&[RecoveredCheckpointMetric])` helper builds `CheckpointValues` with kind/edit_kind/ts/
file_path/lines and the recovery metadata JSON, and `EventAttributes` carrying the
recovered session id + new trace id, then calls `crate::metrics::record`. One event per
recovered file (consistent with the existing one-event-per-file checkpoint convention,
`daemon/checkpoint.rs:364-382`).

---

## Data flow (end to end)

```
PreToolUse(bash)  ──hook──▶ orchestrator.execute_pre_bash_call
                              ├─ extract command string (preset)
                              ├─ stamp start_ns (wall clock)
                              └─ BashSessionStart{..,command,start_ns} ──socket──▶ daemon
                                                                                   ├─ in-mem session (unchanged)
                                                                                   └─ bash_checkpoints_db.record_start
PostToolUse(bash) ──hook──▶ orchestrator.execute_post_bash_call
                              ├─ stamp end_ns
                              └─ BashSessionEnd{..,end_ns} ──socket──▶ daemon
                                                                       └─ bash_checkpoints_db.record_end

git commit ──▶ daemon post_commit
                 ├─ build AuthorshipLog
                 ├─ fill_unattributed_lines (no-hooks agents, unchanged)
                 ├─ recover_attribution(log, ctx, [BashCorrelationSolver, AiEdgeExtensionSolver])
                 │    ├─ Solver1: lstat untracked files → find_candidates(±3s) → cover lines
                 │    └─ Solver2: extend AI edges into adjacent unknown runs
                 ├─ emit_recovered_metrics(...)   (recovered_bash / extension events)
                 ├─ transform(log)
                 └─ write note
```

---

## Error handling

- All DB operations are best-effort: failures are logged via `tracing`/`eprintln` and the
  commit/checkpoint proceeds. The DB never blocks attribution (matches notes-db posture).
- Solvers are individually fallible and isolated: a panic-free solver returning empty on
  any internal error must not abort the others or the note write. The orchestrator wraps
  each solver call and on `Err` logs + skips.
- `lstat` failures (deleted/renamed files) → that file is skipped by Solver 1.
- Retention pruning failure is non-fatal.

---

## Testing strategy (strict TDD)

### Unit — `bash_checkpoints_db.rs` (mirror notes/db.rs tests)
- fresh DB creates table + version row = 1.
- `record_start` then `record_end` round-trips; `end_ns` set.
- `record_start` upsert on duplicate `(session_id, tool_use_id)` does not duplicate rows.
- `find_candidates` returns rows overlapping the window; excludes other repos; excludes
  out-of-window rows; treats NULL `end_ns` as a point at `start_ns`.
- `prune_old` deletes rows older than 30d, keeps newer; rate-limited.
- `database_path` honors `GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH` and default path segments.

### Unit — recovery solvers
- `unknown_lines` helper: matches `background_agent` semantics (shared logic).
- Solver1: file mtime within ±3s of a candidate end → covers all unknown lines to that
  session, emits metric with correct fields; mtime outside window → no recovery; ctime
  fallback when mtime misses; closest-candidate selection among several; cross-repo
  candidate ignored.
- Solver2: unknown run directly below an AI block → absorbed (new trace id, same session);
  unknown run sandwiched between two AI blocks → absorbed to larger neighbor; unknown run
  adjacent to KnownHuman only → untouched; isolated unknown run with no AI neighbor →
  untouched.
- Metric attribute: `attribution_recovery_metadata` round-trips through
  `to_sparse`/`from_sparse`.

### Integration — `tests/integration/` (TestRepo, real daemon)
Use the explicit custom-checkpoint flow (CLAUDE.md) since exact checkpoint ordering and
timing matter:
- **Bash recovery happy path:** write a file via a mocked bash session (record a bash
  checkpoint with start/end bracketing the file mtime), commit with the file's lines
  untracked, assert post-commit the lines are AI-attributed to the bash session and a
  `recovered_bash` event would be emitted (assert via authorship log session record).
- **Bash recovery timing miss:** file mtime far from any bash window → stays untracked.
- **Edge extension:** AI block with a trailing untracked blank line / adjacent untracked
  line → line becomes AI after commit.
- **Edge no-steal:** untracked line adjacent only to human lines → stays untracked.
- **Ordering:** bash solver runs before edge solver; a file recovered by bash isn't
  re-touched by edge.

### Test infra additions
- `TestRepo` helper to seed a bash checkpoint row (via a `git-ai checkpoint` mock path or
  direct control message) and to set/inspect the bash-checkpoints DB path.
- A `mock_bash` checkpoint mock (parallel to `mock_ai`/`mock_known_human`) that drives the
  pre/post bash session control messages with a controllable command + timestamps, so
  tests don't depend on real filesystem-walk timing.

### Expected fallout (Solver 2)
Existing tests that assert untracked lines at the *edge* of AI-attributed code will flip
to AI. These are updated after the core TDD pass (too many to enumerate up front, per the
goal). We grep for `.unattributed_human()` / `.human()` assertions adjacent to `.ai()` in
the affected suites and adjust to the new extension semantics.

---

## Feature flag

Add `attribution_recovery` (default **true** debug & release) and
`bash_checkpoint_tracking` (default **true**) via `define_feature_flags!`
(`src/feature_flags.rs`) so each half can be disabled independently if a regression
appears, and so the recovery pipeline can be force-off in specific tests. Solvers honor
their flag; the DB write path honors `bash_checkpoint_tracking`.

---

## DRY / reuse summary

- DB module copies the `notes/db.rs` skeleton (singleton, migrations, pragmas, test path).
- Retention reuses the metrics-db rate-limited prune pattern.
- `unknown_lines` extracted and shared between `background_agent` and the recovery
  orchestrator.
- `StatEntry::from_metadata` reused for mtime/ctime reads.
- `generate_session_id` / `generate_trace_id` / `AttestationEntry` / `SessionRecord` reused
  for covering lines (same machinery `background_agent` uses).
- Command extraction centralized in one `parse::bash_command_from_tool_input` helper.
- Metric emission reuses `CheckpointValues` + `crate::metrics::record`.

## Open implementation details (resolved during build)
- Exact wall-clock stamping site on the client (pre/post hook) vs. deriving from the
  in-memory `StatSnapshot.taken_at` — prefer explicit client wall-clock to keep ns
  semantics consistent with file times.
- Whether edge extension should also run when a file was just recovered by Solver 1
  (decision: yes — Solver 1 covers the *whole file*, so Solver 2 finds nothing left,
  making this naturally idempotent).
