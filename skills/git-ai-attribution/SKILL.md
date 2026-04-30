---
name: git-ai-attribution
description: "Understand or modify the git-ai attribution system. Use when working on the checkpoint pipeline, working log storage, authorship note format, line attribution logic, stats calculation, move detection, or the pre/post-commit hooks."
argument-hint: "[what you want to understand or change about the attribution system]"
allowed-tools: ["Read", "Grep", "Glob", "Bash", "Edit", "Write"]
---

# git-ai Attribution System

## The Big Picture: Three-Stage Pipeline

```
[1] Checkpoint fires (pre or post AI edit)
       ↓
[2] Working log entry appended to .git/ai/working_logs/<HEAD_sha>/
       ↓
[3] git commit → post-commit hook → AuthorshipLog written to refs/notes/ai
```

## Stage 1: Checkpoints

### Checkpoint Types

| Kind | `to_str()` | Who fires it | Attribution result |
|---|---|---|---|
| `Human` (legacy) | `"human"` | AI agent presets *before* they edit a file | "Untracked" — the sentinel `"human"` author_id is dropped in the final note; shows as `unknown_additions` in stats |
| `KnownHuman` | `"known_human"` | IDE/editor extensions on real keystrokes | Author hash `h_<14 hex>` routed to `metadata.humans` |
| `AiAgent` | `"ai_agent"` | AI agent presets *after* they edit a file | Hash `<16 hex>` of `SHA256("{tool}:{agent_id}")` routed to `metadata.prompts` |
| `AiTab` | `"ai_tab"` | Tab-completion AI | Same hashing as `AiAgent` |

**The AI pre/post duality:** AI agent presets fire a `Human` (untracked) checkpoint *before* editing to baseline the file state, then fire an `AiAgent` checkpoint *after* editing. The diff between the two is the AI's contribution.

### Checkpoint Command

```
git-ai checkpoint <preset> [<file>...]
```

`<preset>` is matched to `AgentCheckpointPreset` implementations in `src/commands/checkpoint_agent/agent_presets.rs`. Test-only presets: `mock_ai`, `mock_known_human`, `human` (bare/legacy).

The checkpoint command in `src/commands/checkpoint.rs`:
1. Resolves the working directory and touched file paths
2. Loads prior checkpoints from the working log to get previous file states
3. Per file: SHA256-hashes current content, loads prior attributions, runs `AttributionTracker::update_attributions_for_checkpoint`
4. Builds a `Checkpoint` struct and appends it to the JSONL working log

## Stage 2: Working Log

Storage: `.git/ai/working_logs/<base_commit_sha>/` (keyed by HEAD at checkpoint time)

```
Checkpoint                              (working_log.rs)
 ├─ kind: CheckpointKind
 ├─ entries: Vec<WorkingLogEntry>
 │   ├─ file: String                   (POSIX-normalized path)
 │   ├─ blob_sha: String               (SHA256 of file content)
 │   ├─ attributions: Vec<Attribution>       (char-level)
 │   └─ line_attributions: Vec<LineAttribution>  (line-level)
 ├─ agent_id: Option<AgentId { tool, id, model }>
 ├─ transcript: Option<AiTranscript>
 └─ line_stats: CheckpointLineStats
```

**INITIAL bucket:** `.git/ai/working_logs/<sha>/INITIAL/` carries uncommitted AI-attributed lines forward across commits, so partial staging doesn't lose attribution when the user commits in multiple batches.

## Stage 3: AuthorshipLog (refs/notes/ai)

### Schema Version: `authorship/3.0.0`

```
AuthorshipLog
 ├─ attestations: Vec<FileAttestation>
 │   ├─ file_path: String
 │   └─ entries: Vec<AttestationEntry>
 │        ├─ hash: String           ("h_" prefix → human; otherwise → AI prompt hash)
 │        └─ line_ranges: Vec<LineRange>
 └─ metadata: AuthorshipMetadata
     ├─ schema_version: "authorship/3.0.0"
     ├─ base_commit_sha: String
     ├─ prompts: BTreeMap<hash, PromptRecord>
     │    └─ PromptRecord { agent_id, messages, total_additions/deletions, accepted_lines, ... }
     └─ humans: BTreeMap<"h_"+hash, HumanRecord>
```

### Wire Format

The note is a hybrid text+JSON document:
```
src/file.rs
  abc123def4567890 1,2,19-222
  h_31dce776f88375 5
"path with space.rs"
  fedcba9876543210 400-405
---
{ ...AuthorshipMetadata as pretty-printed JSON... }
```

`---` divides attestation section from metadata JSON. Quoted paths cannot contain literal quotes. Line ranges sort ascending.

### Hash Conventions

- AI prompt hash: first 16 hex chars of `SHA256("{tool}:{agent_id}")`
- Human hash: `"h_"` + first 14 hex chars of `SHA256(git_committer_identity)` → always 16 chars total
- The `h_` prefix is load-bearing — routes entries to `metadata.humans` instead of `metadata.prompts`

### Git Notes Namespace

All authorship data lives under `refs/notes/ai` — NOT the default `refs/notes/commits`.

```bash
git notes --ref=ai list                    # list all notes
git log --notes=ai                         # show notes in git log
git notes --ref=ai show <commit_sha>       # show one note
```

## AttributionTracker — The Core Engine

`src/authorship/attribution_tracker.rs`

`update_attributions_for_checkpoint(old_content, new_content, old_attrs, author, ts, is_ai)` runs a 5-phase pipeline:

1. **Diff**: line-level via `imara_diff`, descend into changed hunks. Fast path for huge hunks (≥256KB); tokenized diff for sub-line precision otherwise. AI checkpoints always force Delete+Insert (never Equal) so coincidental token matches can't inherit prior human attribution.

2. **Catalog**: scan diff ops into `Vec<Deletion>` + `Vec<Insertion>` with byte offsets.

3. **Move detection** (skipped for AI checkpoints): finds moved code blocks ≥3 lines, preserves original authorship through moves.

4. **Transform**: `Equal` → carry forward; `Delete`+move → reattach; `Insert`(AI) → `current_author`; `Insert`(non-AI) → inherit from context or last attribution.

5. **Merge**: sort, dedup, coalesce contiguous same-author runs.

## Move Detection

`src/authorship/move_detection.rs`

Algorithm: group consecutive lines, build content hash lookup, greedily extend matches ≥ threshold (default: 3 lines). Whitespace-normalized matching. **Always skipped for AI checkpoints** — AI rewrites should attribute new bytes to AI, not preserve through-moves.

## VirtualAttributions — The Pivot Type

`src/authorship/virtual_attribution.rs`

In-memory holder of per-file line attributions before they become a persisted note. Constructors:

| Constructor | Source | Use case |
|---|---|---|
| `from_just_working_log` | Working log checkpoints (live worktree) | Normal post-commit |
| `new_for_base_commit` | git blame on a commit + notes | Historical range stats |
| `from_working_log_snapshot` | Frozen final-state map | Daemon mode (avoids TOCTOU) |
| `from_working_log_for_commit` | Blame + working log merged | Amend/squash |

`to_authorship_log_and_initial_working_log` is the central conversion — called in post-commit to produce the final `AuthorshipLog` from the VirtualAttributions.

## Pre-commit and Post-commit Hooks

`src/authorship/pre_commit.rs` — minimal:
- Detects if an AI agent's bash tool is active (`bash_tool::checkpoint_context_from_active_bash`)
- If active: fires `AiAgent` checkpoint on staged files
- Otherwise: fires `Human` (untracked) checkpoint

`src/authorship/post_commit.rs` — the heavy lifter:
1. Refresh prompt transcripts (`update_prompts_to_latest`)
2. Batch-upsert `PromptDbRecord` rows to SQLite
3. Build `VirtualAttributions::from_just_working_log`
4. Run `to_authorship_log_and_initial_working_log` → final `AuthorshipLog`
5. Apply `PromptStorageMode` (Local / Notes / Default→CAS)
6. Serialize and `notes_add(repo, commit_sha, ...)` → `refs/notes/ai`
7. Skip stats if too expensive (>1000 hunks, >6000 lines, >200 files, or merge commit)
8. Write new INITIAL attributions; delete parent's working log directory
9. Emit `committed` telemetry metric

## Stats Calculation

`src/authorship/stats.rs`

`CommitStats` fields: `human_additions`, `unknown_additions`, `ai_additions`, `ai_accepted`, `total_ai_additions/deletions`, `time_waiting_for_ai`, `tool_model_breakdown`.

Pipeline: git diff stats → per-file, intersect attestation line ranges with diff added-lines using binary-search overlap → bucket by `h_` prefix. `time_waiting_for_ai` sums User→Assistant message timestamp deltas in the transcript.

## Coordinate Space Discipline

**Working log line numbers = workdir coordinates** (lines in the file as it exists on disk)
**AuthorshipLog line numbers = commit coordinates** (lines in the file as committed)

Conversion in `to_authorship_log_and_initial_working_log`: count unstaged lines below each candidate line and subtract. This is why post-commit must compute both `committed_hunks` (from `git diff parent..commit`) and `unstaged_hunks` (from `git diff commit..worktree`) before converting coordinates.

## Key Invariants

- **Force-split for AI**: AI checkpoints emit Delete+Insert (never Equal), so coincidental token matches can't inherit prior human attribution.
- **Move detection is non-AI only**: AI rewrites attribute new bytes to AI; moves are only useful for human edits.
- **INITIAL bucket continuity**: uncommitted AI-attributed lines carry forward across commits via the INITIAL directory so partial staging loses nothing.
- **`h_` prefix partitions the hash space**: one 16-char hash space serves both AI and human attribution without collision.
- **NFC normalization** at every path comparison boundary (working-log paths may be NFD from git on macOS).
- **POSIX normalization** (`normalize_to_posix`) at every checkpoint entry boundary.

## Relevant Files

| File | Purpose |
|---|---|
| `src/commands/checkpoint.rs` | Checkpoint command entry point |
| `src/commands/checkpoint_agent/agent_presets.rs` | Per-agent preset implementations |
| `src/authorship/working_log.rs` | Working log storage, Checkpoint/Attribution types |
| `src/authorship/attribution_tracker.rs` | Core diff-and-attribute engine |
| `src/authorship/virtual_attribution.rs` | In-memory attribution pivot |
| `src/authorship/authorship_log.rs` | AuthorshipLog type |
| `src/authorship/authorship_log_serialization.rs` | Wire format, hash generation, schema version |
| `src/authorship/pre_commit.rs` | Pre-commit hook |
| `src/authorship/post_commit.rs` | Post-commit hook (attribution finalization) |
| `src/authorship/stats.rs` | CommitStats calculation |
| `src/authorship/move_detection.rs` | Move detection algorithm |
| `src/authorship/range_authorship.rs` | Commit-range attribution stats |
| `src/authorship/prompt_utils.rs` | Prompt lookup and transcript refresh |
| `src/git/refs.rs` | `notes_add`, `get_authorship`, `grep_ai_notes` |
