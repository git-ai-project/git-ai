# Trace2 Rewrite Ops, Attribution Fuzzer, and Daemon Ingestion Spec

Status: current design and implementation notes for the trace2 rewrite-ops work on
`feat/attr-fuzzer-v2`.

This document is intentionally both a spec and a postmortem. It records what the
current design is trying to guarantee, what is working, what is not fully proven,
and which attempted approaches were rejected.

## Executive Summary

The rewrite-ops rewrite is based on one core idea: attribution should follow
immutable Git object data, not the live working tree. For commits and history
rewrites, the durable inputs are commit SHAs, tree SHAs, Git notes, working-log
snapshots persisted by checkpoints, and ordered ref-log entries bounded by a
known cursor. The live worktree is valid only at checkpoint time, because that is
the moment a checkpoint intentionally snapshots mutable user state.

The implementation has moved the rewrite paths in the right direction:

- Commit authorship uses persisted working-log data plus committed tree data.
- Rebase, amend, update-ref style restacks, cherry-pick, revert, squash merge,
  and reset share a smaller note-shifting model based on commit mappings and
  `diff-tree`.
- Reset reconstruction and stash restoration avoid reading the user's live
  worktree after the operation has completed.
- Notes and diffs are batched in the rewrite core, avoiding the worst
  per-commit/per-file git spawn patterns.
- The fuzzer exists and is useful as invariant pressure, but it is not yet a
  complete proof harness for every rewrite workflow.

The daemon trace2 ingestion work is more nuanced. The correct boundary is a
pre-command ref cursor. If the daemon had a cursor before a ref-moving command,
it can consume exactly the reflog entries appended by that command. If it did
not have a cursor before the command, stock Git trace2 does not provide enough
information to exactly identify the command's ref transition later. In that
case, the correct behavior is to fail closed for attribution for that command,
then observe the current reflog end as the baseline for future commands.

The current dirty worktree includes partially implemented/in-progress cursor
changes and tests around this boundary. Some focused suites have passed in prior
runs, but `daemon_mode` had known failures during this work. Do not treat the
current branch as fully proven until those failures are resolved and the full
test gates are green.

## First Principles

### 1. Git object data is the source of truth

For committed history, the source of truth is immutable object data:

- commit SHAs
- tree SHAs
- blob contents addressed by a treeish and path
- Git notes under `refs/notes/ai`
- reflog entries only when bounded by a known cursor

Any algorithm that asks "what does the worktree contain now?" after a Git
operation has completed is race-prone. A user or test can mutate the file after
Git exits but before the daemon processes the trace2 event. mtime filtering does
not make this safe.

### 2. Checkpoints are the only valid live-worktree snapshot point

`git-ai checkpoint` exists specifically to snapshot mutable file state at a
known point in the editing workflow. It is valid for checkpoints to read files.
It is not valid for delayed daemon rewrite handling to read files from the
current worktree and pretend that state is the state from an earlier Git
operation.

### 3. Reflog ordering is useful; reflog timestamps are not a correctness
boundary

Raw reflog entries have this form:

```text
<old> <new> <author> <timestamp> <tz>\t<message>
```

The timestamp is seconds-resolution and is not causally linked to a trace2 root.
Two commits can share the same reflog timestamp. Messages can also collide.
Therefore timestamps and messages are diagnostics or filters only; they are not
formal ownership proof.

The exact reflog data is:

- append order
- byte offsets
- `old` and `new` OIDs
- a complete-line boundary
- an anchor proving a saved offset still points into the same reflog generation

### 4. No cursor means no exact delayed ownership

Stock Git trace2 does not emit the created commit SHA or the ref update OIDs for
normal `git commit`. A delayed trace2 root that says "git commit ran" cannot be
formally matched to a particular reflog line if the daemon did not know the
pre-command reflog position.

The exact behavior is:

- If a pre-command cursor exists, consume entries after that cursor.
- If no pre-command cursor exists, do not infer ownership from latest HEAD,
  message matching, or timestamp matching.
- After processing an unresolved command, observe the current reflog end as the
  baseline for future commands. This does not attribute the unresolved command.

### 5. Conflict resolution is new work, not magic preservation

When a user or AI resolves a conflict, any new content in the resolution should
come from checkpoint data around that resolution. Preserved lines from old
commits should retain attribution from their original commits. If the resolution
rewrites a line, the rewritten line is not the old line. It must be attributed
from the resolution checkpoint if one exists, or remain unattributed.

## Rewrite-Ops Rewrite

### Public entrypoint

The current rewrite core is centered on `src/authorship/rewrite.rs`:

```rust
pub enum RewriteEvent {
    NonFastForward {
        old_tip: String,
        new_tip: String,
        onto: Option<String>,
    },
    CherryPickComplete {
        sources: Vec<String>,
        new_commits: Vec<String>,
    },
    SquashMerge {
        source_head: String,
        squash_commit: String,
        onto: String,
    },
}

pub fn handle_rewrite_event(repo: &Repository, event: RewriteEvent) -> Result<(), GitAiError>
```

The intended shape is good: daemon command detection should normalize Git
operations into one of these events, and all commit-note migration should flow
through this entrypoint.

### Core note-shifting algorithm

For each `(source_commit, destination_commit)` mapping:

1. Batch-read source and destination notes with `notes_api::read_notes_batch`.
2. Resolve unique commit SHAs to tree SHAs in one `git rev-parse` call.
3. Run one `git diff-tree --stdin -p -U0 -M --no-color -r` for all pairs.
4. Parse hunks and renames.
5. Shift line ranges that survive outside diff hunks.
6. Drop attribution ranges that fall inside modified hunks.
7. Update `metadata.base_commit_sha` to the destination commit.
8. Batch-write destination notes with `notes_api::write_notes_batch`.

This gives a simple semantic rule:

- unchanged lines keep their attribution, shifted by Git's diff
- renamed files keep attribution under the new path
- modified lines are not assumed to preserve attribution
- conflict-resolution lines need checkpoint evidence from the resolution itself

This is the correct default. It leans on Git's diff as the reality for how old
and new trees relate.

### Non-fast-forward rewrites

`RewriteEvent::NonFastForward` handles rebases, amended histories, restacks, and
other branch rewrites where an old tip is replaced by a new tip.

Implementation path:

- Find merge base of `old_tip` and `new_tip`.
- If `base == new_tip`, this is a backward reset. Reconstruct working log for
  the reset case instead of writing notes.
- If `base == old_tip`, this is a fast-forward and no rewrite note migration is
  needed.
- Otherwise, run `git range-diff` over the old and new ranges.
- Parse range-diff output to derive `(old_commit, new_commit)` mappings.
- Add merge-commit mappings where needed.
- Shift notes for those mappings.

What is good:

- The core is object-based.
- It uses Git range-diff rather than inventing a patch matching algorithm for
  rebases.
- It batches note reads, note writes, and tree diffs.
- It cleanly handles dropped/squashed commits by mapping multiple source commits
  into the destination commit when range-diff indicates that relationship.

Known sensitivity:

- `range-diff` is still an algorithmic Git operation, not a trace2 boundary. It
  is appropriate for mapping immutable old and new commit ranges once those
  ranges are known exactly.
- The exactness risk is not in range-diff itself; it is in how the daemon
  determines `old_tip`, `new_tip`, and `onto`.

### Rebase

Rebase detection is daemon-side. The cursor consumes `HEAD` and branch reflog
entries bounded by known ref cursors and emits ref changes. The side-effect layer
collapses the relevant ref changes and calls `handle_non_fast_forward_rewrite`.

For failed/conflicted rebase:

- The initial failed rebase can move HEAD to the onto commit and leave rebase
  state active.
- Later `rebase --continue` may look like a fast-forward from onto to new tip.
- The daemon stores pending original-head/onto information when it can be known
  exactly, then uses it on continue/skip completion.

Conflict resolution:

- Existing source attribution is shifted into the new commit.
- Resolution checkpoint data is merged into the final note using
  `conflict_resolution`.
- Preserved old lines should retain original attribution.
- Newly written resolution lines should be attributed to the resolving AI/human
  if checkpointed, otherwise remain unattributed.

What works according to current tests:

- Regular rebase preservation is covered in integration tests.
- Pull-rebase preservation and autostash paths are covered.
- Real-world rebase conflict tests exist.
- Cold mid-rebase continue tests cover daemon restart after a raw traced-disabled
  failed rebase and then traced continue with AI resolution.

What remains sensitive:

- The daemon must not use stale rebase reflog history from prior commands.
- The `onto` hint must be command-owned, not guessed from latest repo state.
- Any path that reads `.git/rebase-merge` or `.git/rebase-apply` after the fact
  should be treated suspiciously unless it is only reading current in-progress
  state for a command that is actually in progress. For delayed completed
  commands, that state is mutable and may be gone.

### Cherry-pick

`RewriteEvent::CherryPickComplete` maps source commits to newly created commits.

Current source pairing logic:

- Resolve explicit source OIDs from command arguments where possible.
- Use cursor-bounded reflog entries to identify destination commits.
- Use `rewrite_cherry_pick::match_cherry_pick_pairs`:
  - pass 1: stable patch-id anchoring
  - pass 2: positional gap-fill for remaining unmatched commits

What is good:

- Patch-id anchoring is better than pure positional matching for clean picks.
- The final note shifting uses the same batched core as other rewrites.
- Conflicted cherry-picks can use resolution checkpoint notes on the destination.

What is not perfect:

- Symbolic source refs are mutable if resolved later. They are exact only if
  resolved from command-time data or if the argument itself is an immutable OID.
- `--no-commit` is fundamentally different: it changes the index/worktree, not
  commits. It should not synthesize committed attribution from the current index
  after arbitrary delay unless the exact index/tree created by the command is
  captured from immutable data.
- Positional gap-fill is a pragmatic mapping once exact source and destination
  sequences are known. It should not be used to compensate for unknown command
  ownership.

### Reset

Backward reset reconstructs the working log instead of creating notes.

Current implementation:

- List commits being unwound: `new_tip..old_tip`.
- Batch-read their authorship notes.
- Shift intermediate commit attributions into the old tip's coordinate space.
- Batch-read file contents from `old_tip` and `new_tip` trees.
- Only reconstruct files whose old-tip content differs from the reset target.
- Write `INITIAL` working-log data under the new base commit.
- Do not read the live worktree.
- Do not clear `checkpoints.jsonl`, because checkpoints may have been appended
  after the reset but before the daemon side effect runs.

This is a first-principles fix for the reset race class. The reset operation
undoes commits into staged/working state, and the only stable snapshot for that
state is the old tip tree plus notes from the undone commits.

### Squash merge

Squash merge is a commit rewrite from many source commits into one destination
commit.

Current implementation:

- `RewriteEvent::SquashMerge` takes `source_head`, `squash_commit`, and `onto`.
- Determine merge base of source and onto.
- List source commits from base to source head.
- Fetch/read all source notes.
- Shift each source note into source-head coordinate space if needed.
- Merge all shifted source logs.
- Shift merged log from source head to the squash commit.
- If the squash commit already has a resolution note, merge source preservation
  with resolution attribution.
- If there is a working log on `onto`, post the squash resolution working log
  with a transform that merges source attribution.

Recent cold-start nuance:

- A traced `merge --squash <immutable-oid>` can record the source exactly from
  the command argv even without a prior cursor.
- A traced `merge --squash feature` cannot be recovered exactly after delay if
  `feature` moves. That must fail closed or require a pre-command cursor.

What is good:

- The note transformation is object-based and batched.
- Conflict/resolution attribution can be merged instead of overwritten.
- Immutable source OID support covers a useful exact cold-start case.

Known limitation:

- Symbolic squash sources without a cursor are not exactly recoverable from
  stock trace2 after the fact.

### Stash

Stash is not a commit-note rewrite. It migrates working-log data across stash
create/apply/pop/drop.

Current implementation direction:

- On stash create, save metadata keyed by stash SHA:
  - base commit
  - timestamp as metadata only
  - pathspecs
- Copy relevant working-log data into `.git/ai/stashes`.
- Clean stashed paths from the original working log.
- On apply/pop, restore copied working-log data to the target head.
- If the stash was created on a different base, shift via a reconstructed stash
  application using an isolated temporary index/worktree.
- The isolated reconstruction reads contents through a produced tree and
  `batch_read_paths_at_treeishes`, not from the user's current worktree.

This addresses the earlier bad race where stash restoration read live worktree
files after `stash pop/apply`.

Remaining caution:

- The temporary isolated worktree is an implementation tool. It must be isolated
  from user hooks and must not cause trace2 recursion or daemon side effects.
- Stash target selection must be cursor-bounded. `stash@{N}` is mutable if
  resolved after later stash operations.

### Revert

Revert is handled separately in `rewrite_revert.rs`.

The model is:

- A revert can restore lines deleted by an earlier commit.
- The restored line's attribution should come from the commit where that content
  previously existed.
- The implementation uses source commit/base data and note shifting rather than
  treating restored lines as human.

Regression tests cover restoring AI attribution from older commits and shifted
line-number cases.

## Daemon Trace2 Ingestion

### Desired architecture

The daemon has three separate jobs:

1. Normalize trace2 roots into `NormalizedCommand`.
2. Sequence commands per repository family.
3. Apply side effects from exact command facts.

The normalizer should be race-free. It should not read mutable repository state
to inject missing command facts. It may parse trace2 payloads, argv, command
names, def_repo, exit/atexit status, and explicit daemon-provided test metadata.

The family actor owns ref cursors. That ownership matters because cursor state
is per repo family and must be updated in the same order as command side effects.

### Current data path

Trace2 ingestion:

- socket listener accepts trace2 JSON frames
- `prepare_trace_payload_for_ingest` filters definitely read-only roots
- mutating roots are enqueued with a sequence number
- `TraceNormalizer` groups frames by root sid
- terminal root events produce a `NormalizedCommand`
- the family coordinator sequences commands by family
- `RefCursor::enrich_command` consumes reflog entries and fills
  `cmd.ref_changes`
- the daemon side-effect layer applies commit/rewrite/stash/pull/push behavior

Current `NormalizedCommand` includes:

- family/worktree
- root sid
- raw argv
- primary/invoked command
- exit code
- start/finish trace timestamps
- optional `reflog_start_offsets`
- command-specific fields such as stash target and cherry-pick source OIDs
- `ref_changes`

### Correct cursor semantics

The cursor stores:

- per-ref byte offset
- anchor for the reflog record ending at that offset
- consumed offsets and anchors
- in-memory stash stack
- pending cherry-pick source OIDs

Cursor reads must handle:

- reflog file missing
- reflog truncated/pruned
- partial line at end of file while Git is writing
- branch delete/recreate causing a new reflog generation
- worktree HEAD reflogs and common refs having different paths

Important implementation details:

- `read_reflog_records` ignores incomplete trailing lines.
- `read_reflog_record_ending_at` validates that a saved offset ends at newline.
- anchors are checked before reusing saved offsets.
- if offset is beyond file length, the cursor is cleared.
- if the anchor no longer matches, the cursor is cleared.
- branch delete/recreate tests ensure stale cursor offsets do not poison new
  branch generations.

### What works

The cursor model is the right abstraction for delayed daemon processing when a
cursor exists before a command:

- It is independent of mtimes.
- It does not read the live worktree.
- It relies on append order and OIDs.
- It can detect truncation/rewrite of the reflog.
- It can fail closed when a cursor is invalid.

Checkpoint ordering is also conceptually right:

- A checkpoint is a real causal observation point.
- When a checkpoint reaches the family actor, `observe_checkpoint_worktree`
  seeds cursor boundaries for HEAD/common refs/stash.
- A later ref-moving command can then be matched exactly from that boundary.

### What does not work with stock trace2

Stock trace2 does not emit the new commit SHA for `git commit`. It also does not
emit a complete command-owned ref transition for all ref-moving commands.

Therefore these are not exact:

- "latest HEAD after daemon sees the event"
- "the reflog entry whose message matches the commit message"
- "the reflog entry with a timestamp near the trace2 finish time"
- "the first reflog entry after daemon ingestion noticed the root"
- "resolve symbolic args after the command has completed"

All of those can be wrong if the daemon is delayed or the user runs another Git
command quickly.

### The correct fail-closed rule

For ref-moving commands:

- if cursor exists: use it
- if command payload contains exact start offsets from a trusted in-process
  source: use them
- if command argv contains immutable OIDs sufficient for the operation: use them
  where applicable
- otherwise: do not attribute the command

After an unresolved command, the family actor may observe the current reflog end
as a future baseline. This is not a fallback for the current command. It is only
how future commands become exact again.

### Failed and rejected approaches

#### mtime-guarded worktree snapshots

The original carryover snapshot race came from reading mutable worktree files
after Git exited, guarded by `mtime <= git_finish_time`.

Why it failed:

- filesystems can have coarse timestamp resolution
- a later edit can land in the same timestamp quantum
- the daemon can process trace2 asynchronously after later user edits
- the wrong content can be captured as if it were the committed content

This approach is rejected entirely. The working log plus commit tree is enough
for post-commit handling.

#### Live worktree stash restore

Earlier stash restoration read files from the current worktree after stash
apply/pop. That is the same race in another form.

Current direction is better:

- use saved stash working-log data
- reconstruct applied content from stash object plus target head in isolation
- write restored attribution into the target working log

#### Daemon-ingress synthetic reflog offsets

An attempted solution captured reflog offsets in daemon ingress when the daemon
first saw a trace2 root. This was appealing because it looked like a pre-command
boundary, but it was not one.

Why it failed:

- The daemon sees trace2 frames asynchronously.
- By the time the daemon reads the reflog, Git may already have appended the
  command's ref update.
- Capturing "start offsets" in the daemon is therefore often a post-command
  offset.
- Tests that inject those offsets can accidentally model a capability stock
  trace2 does not provide.

This approach should not be used in production. The current dirty work removes
daemon-ingress synthesis and adds tests asserting ingress does not synthesize
offsets. Some tests still inject `git_ai_root_reflog_start_offsets` as synthetic
metadata; those should be treated as tests for a hypothetical trusted boundary,
not stock trace2 behavior.

#### Trace2 barrier / hidden read-command sync

A trace2 barrier was explored to force read commands to wait for prior trace2
traffic. That was rejected.

Why it was wrong:

- Production read commands should not perform hidden daemon syncs.
- Tests already sync explicitly before assertions.
- It changes timing behavior and can hide races instead of modeling them.
- It does not solve the no-cursor exactness problem; it just makes the daemon
  more likely to catch up before a read.

The right place to wait is explicit test/assertion synchronization or explicit
checkpoint sequencing, not arbitrary production reads like `show` or `blame`.

#### Reflog timestamp matching

Reflog timestamps are seconds-resolution. They cannot distinguish same-second
commands and are not causally linked to trace2 roots. They may be useful for
debug logs, but they must not be used as proof of command ownership.

#### Message matching without a cursor

Commit messages and reflog messages collide in real workflows. Duplicate commit
messages are common. Matching by message without a cursor can attribute the
wrong commit. The duplicate-message cold tests encode this.

## Cold-Start Behavior

"Cold" means the repo has existing history, but the daemon has no cursor because
the setup happened with trace2 disabled or before the daemon knew about the repo.

Correct expectations:

- The first traced command should be processed as a Git operation.
- The daemon should not crash, deadlock, or poison future state.
- If the first traced command lacks exact attribution evidence and there is no
  pre-command cursor, it should fail closed for authorship.
- After that command, the daemon can observe the current reflog end and future
  commands can be exact.

Examples:

- First traced commit in a cold repo: process command, but do not guess commit
  ownership if no checkpoint/cursor exists.
- Traced commit after prior traced baseline: can create an empty authorship note
  or AI note depending on working-log evidence.
- Duplicate message command after untraced same-message commit: fail closed.
- Cold traced `merge --squash <sha>` can preserve source attribution because the
  source is immutable in argv.
- Cold traced `merge --squash branch-name` is not exactly recoverable if the
  branch can move before daemon processing.

## Fuzzer Spec

### Purpose

The fuzzer should prove attribution invariants across long sequences of edits
and rewrite operations. It is not meant to replace deterministic regression
tests. It should find failures, log enough data to reproduce them, and then each
found failure should be converted into a targeted TestRepo regression.

### Current implementation

Files:

- `tests/integration/fuzzer/model.rs`
- `tests/integration/fuzzer/operations.rs`
- `tests/integration/fuzzer/engine.rs`
- `tests/integration/fuzzer/mod.rs`

Model:

- Each file line is represented by a unique Unicode char.
- `AttrRegistry` records the checkpoint-time attribution for every char.
- `FileModel` holds current line order and expected attribution.
- The model asserts against `git-ai blame`.

Operations:

- random edit
- checkpoint as AI
- checkpoint as known human
- checkpoint as untracked legacy human
- commit
- amend
- rebase
- cherry-pick

Configured runs:

- standard fixed seeds
- rewrite-heavy fixed seeds
- one random seed test
- ignored marathon/chaos tests

What is good:

- The one-char-per-line model makes line identity easy to reason about.
- The operation log is included in assertion failures.
- It asserts every line, not just aggregate counts.
- It pressures long operation sequences that humans do not naturally write.

What is weak:

- The default random test should print its seed or be moved to ignored/nightly.
  A non-reproducible random CI failure is not acceptable.
- The fuzzer currently treats attribution as AI vs not-AI for blame purposes. It
  does not fully distinguish known human vs untracked in every assertion path.
- It mostly uses one file and simple line identities.
- It does not yet deeply model partial staging, multi-file conflicts, stash,
  squash, reset, pull-rebase-autostash, branch lifecycle, reflog pruning, or
  daemon restart at arbitrary points.
- It aborts rebase/cherry-pick conflicts rather than modeling rich resolution
  behavior.

### Desired fuzzer invariants

The fuzzer should eventually assert:

1. AI lines that survive unchanged through Git rewrites remain AI.
2. Known-human lines that survive unchanged remain known human.
3. Untracked lines remain unattributed unless later checkpointed.
4. Lines rewritten during conflict resolution get attribution from the resolution
   checkpoint, not from the source commit.
5. Preserved source-side conflict lines keep source attribution.
6. Preserved target-side conflict lines keep target attribution.
7. Keeping both conflict sides preserves attribution for both sides.
8. Deleted lines do not leave stale attribution ranges.
9. Renamed files preserve surviving line attribution under the new path.
10. Partial staging attributes only the committed content and carries unstaged
    working-log attribution forward.
11. Reset soft/mixed reconstructs working-log attribution from undone commits.
12. Stash push/pop/apply preserves uncommitted attribution without reading the
    user's live worktree after the fact.
13. Cold-start no-cursor commands fail closed rather than guessing.
14. Reflog truncation/pruning clears invalid cursors and does not panic or
    consume stale entries.
15. Symlink/canonical path variants resolve to the same family/cursor semantics.
16. No operation requires hidden daemon sync except explicit test assertion sync.
17. The daemon never deadlocks on partial trace2 roots, incomplete reflog lines,
    socket close ordering, or child process trace traffic.

### Desired fuzzer operations

The fuzzer should add operation families for:

- multi-file edits
- file rename/move
- file delete/recreate
- partial staging with unstaged carryover
- commit amend with and without checkpointed edits
- reset soft/mixed/hard/pathspec
- stash push/apply/pop/drop with pathspecs
- clean cherry-pick
- conflicted cherry-pick with resolution modes:
  - keep ours
  - keep theirs
  - keep both
  - rewrite with AI checkpoint
  - rewrite without checkpoint
- clean rebase
- rebase conflict with the same resolution modes
- pull --rebase
- pull --rebase --autostash
- merge --squash with immutable OID source
- branch delete/recreate/rename/copy
- daemon restart between command phases
- delayed trace replay after additional user activity
- cold setup with trace2 disabled followed by traced commands

### TDD rule for fuzzer findings

Every fuzzer failure must produce:

1. A saved seed and operation log.
2. A minimized deterministic TestRepo test.
3. A failing assertion before the fix.
4. A code fix that addresses the underlying class, not just the generated case.
5. The original fuzzer seed retained as coverage if it is not too expensive.

## Current Test Evidence

Representative coverage currently exists in:

- `tests/integration/rewrite_ops_attribution.rs`
- `tests/integration/cold_trace2_repo.rs`
- `tests/commit_tree_update_ref.rs`
- `tests/integration/fuzzer/*`
- `tests/integration/pull_rebase_ff.rs`
- `tests/integration/rebase_realworld.rs`
- `tests/integration/subdirs.rs`
- `tests/daemon_mode.rs`
- unit tests in `src/authorship/rewrite.rs`
- unit tests in `src/authorship/hunk_shift.rs`
- unit tests in `src/daemon/ref_cursor.rs`

Recent focused runs during this work showed:

- ref cursor unit tests passing
- delayed late-offset commit tests passing
- `commit_tree_update_ref` passing
- cold trace2 repo tests passing

But `daemon_mode` had failures after the latest cursor fail-closed changes,
mostly around base commits expecting authorship notes and pull note-push behavior
when `refs/notes/ai` does not exist. Until those are resolved, the whole branch
is not proven.

## What I Think Is Good

The rewrite core is the right shape:

- one entrypoint
- one note-shift mechanism
- Git object data as input
- Git diff as the reality for line movement
- hunk regions invalidated rather than guessed
- resolution checkpoints merged explicitly
- notes/diffs batched

The reset and stash race fixes are first-principles:

- reset uses old/new commit trees and notes, not live worktree
- stash shifted restore reconstructs content from stash/target object data in an
  isolated environment

The cursor model is also the right shape:

- per-family ownership
- byte offsets plus anchors
- complete-line parsing
- pruning/truncation detection
- fail-closed on invalid cursor

## What I Do Not Think Is Fully Solved Yet

### Full proof across all workflows

The goal is "all git workflows." The current tests are broad, but they are not a
complete proof. The fuzzer is not yet rich enough to cover all workflow classes,
and the daemon mode suite has had recent regressions.

### Synthetic reflog offset test artifacts

Some tests still inject `git_ai_root_reflog_start_offsets` into trace payloads.
That models a trusted command-start boundary that stock trace2 does not provide.
Those tests are useful only if clearly labeled as synthetic-boundary tests. They
should not be used to claim stock trace2 can recover first-command ownership in a
cold repo.

### Cold first write command attribution

If there is no cursor and stock trace2 does not include ref transition OIDs, the
first delayed write command cannot be attributed exactly. This is not an
implementation bug; it is missing information. The product decision should be to
fail closed or introduce a real command-start boundary source.

### Symbolic refs after delay

Any logic that resolves `feature`, `HEAD~1`, `stash@{0}`, etc. after a delayed
command must prove that resolution is command-time resolution. Otherwise it is
mutable. Immutable OID argv is safe; symbolic argv is not safe without a cursor
or command-time capture.

### Remaining timestamp use

`src/daemon/ref_cursor.rs` still parses reflog timestamps and uses them in some
direct update-ref correlation paths. That should be audited. Timestamp equality
can be a secondary correlation only when old/new OIDs and a cursor-bounded
window already make the candidate set exact. It must never be the primary proof.

### Random fuzzer in default test set

The current `fuzz_random` test does not print its seed. That is not acceptable
for CI if it can fail. Either print the seed on every run or move random fuzzing
to ignored/nightly.

## Spec Requirements Before Calling This Complete

This work should not be considered complete until all of these are true:

1. No daemon rewrite side effect reads the user's live worktree to reconstruct
   past Git state.
2. No mtime-based guard is used for committed or rewrite attribution.
3. No hidden production read-command sync is used to make tests pass.
4. Ref-moving command ownership uses a cursor, exact OIDs, or fails closed.
5. Reflog timestamps are not used as primary ownership proof.
6. Symbolic refs are not resolved after delay unless bounded by a cursor or
   equivalent command-time fact.
7. Reflog parsing ignores partial trailing lines.
8. Reflog truncation/pruning clears invalid cursors and does not crash.
9. Branch delete/recreate clears stale cursor state.
10. Reset reconstruction is tree/note based and preserves post-reset
    checkpoints.
11. Stash restore is stash-object/target-object based, not live worktree based.
12. Squash merge preserves source attribution for immutable source OIDs and
    fails closed for unrecoverable symbolic cold sources.
13. Conflict resolution attribution is covered for keep-ours, keep-theirs,
    keep-both, AI rewrite, human rewrite, and uncheckpointed rewrite.
14. Partial staging carryover is covered by TestRepo tests and does not rely on
    tests manually writing working logs.
15. Fuzzer failures are reproducible by seed and minimized into deterministic
    regression tests.
16. `task test` passes locally.
17. Ubuntu and macOS CI are green.
18. Windows-specific trace/socket behavior is green or explicitly scoped.

## Recommended Next Steps

1. Finish the cursor fail-closed implementation:
   - resolve the current `daemon_mode` failures
   - make missing `refs/notes/ai` a no-op for note push paths where no notes
     exist
   - ensure first no-cursor traced writes fail closed consistently

2. Remove or quarantine synthetic-offset tests:
   - keep them only if they are explicitly testing a hypothetical trusted
     command-start boundary
   - do not let them imply stock trace2 provides that boundary

3. Audit remaining timestamp use:
   - prove each use is only a tie between already exact candidates
   - otherwise remove it

4. Strengthen the fuzzer:
   - print every seed
   - move pure random to ignored/nightly or make it reproducible
   - add conflict resolution modes
   - add stash/reset/squash/partial-staging/cold-start operation families

5. Add deterministic tests for every race class already found:
   - live worktree after commit
   - live worktree after stash pop
   - delayed duplicate commit messages
   - reflog partial line
   - reflog partially pruned
   - reflog fully pruned
   - symlink/canonical worktree identity
   - delayed symbolic source ref movement

6. Run the gates:
   - focused tests for changed areas
   - `task fmt`
   - `task lint`
   - `task test`
   - CI monitoring for Ubuntu first, then macOS/Windows as required

## Bottom Line

The rewrite-ops core is substantially cleaner and is based on the right data:
commit mappings, Git notes, tree diffs, and checkpoint working logs. The main
remaining risk is not the note-shift algorithm. The main remaining risk is the
daemon's ability to determine exact command ownership from trace2 and reflogs
without inventing data that Git did not provide.

The correct trace2 ingestion answer is strict:

- use a real cursor if one existed before the command
- use immutable OIDs when the command itself contains them
- otherwise fail closed

That is less convenient than trying to recover every cold first command, but it
is the only model here that is exact rather than heuristic.
