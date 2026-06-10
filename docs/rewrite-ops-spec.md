# Rewrite Ops Attribution Spec

Status: current design and implementation notes for the rewrite-ops work on
`feat/attr-fuzzer-v2`.

This document covers the rewrite-ops rewrite: what the system is trying to
guarantee, how the current code works, which parts look solid, and which parts
remain sensitive. It intentionally does not cover the fuzzer or daemon trace2
ingestion in depth; those live in separate docs.

Related docs:

- `docs/attribution-fuzzer-spec.md`
- `docs/daemon-trace2-ingestion-spec.md`

## Summary

The rewrite-ops rewrite is based on one core rule: attribution should follow
immutable Git object data, not the live worktree. For committed history, the
source of truth is commit SHAs, tree SHAs, blob contents, Git notes, persisted
working-log snapshots, and exact command-owned ref transitions.

The rewrite core is in good shape conceptually:

- commit-note migration flows through one small event type
- line movement is derived from Git diffs
- modified hunks are invalidated rather than guessed
- conflict-resolution checkpoints are merged explicitly
- notes and tree diffs are batched
- reset and stash paths avoid the original "read live worktree later" race

The main caveat is not the note-shift algorithm. The main caveat is whether the
daemon has exact `old_tip`, `new_tip`, `onto`, source, and destination facts for
each Git operation. That boundary is documented in
`docs/daemon-trace2-ingestion-spec.md`.

## First Principles

### Git object data is the source of truth

Committed attribution must be derived from:

- source commit notes
- destination commit/tree data
- Git's diff between immutable trees
- persisted working logs written by checkpoints

The live worktree is valid only at checkpoint time. If a daemon side effect runs
after Git exits, it must not open `repo.workdir()/path` and treat that as the
state from the earlier Git command. The user or test may already have changed
the file.

### Git's diff is reality

For rewrite operations, Git's tree diff is the formal relationship between old
and new content. The attribution rule is intentionally simple:

- unchanged lines outside diff hunks retain attribution and shift line numbers
- file renames preserve attribution under the new path
- lines inside changed hunks are not assumed to preserve attribution
- new conflict-resolution content needs checkpoint evidence

This is stricter than trying to infer intent from similar text, but it is much
safer. A line rewritten during conflict resolution is new work.

### Working-log snapshots are command inputs, not fallbacks

Checkpoint working logs are the durable record of uncommitted attribution. They
are expected inputs for:

- normal post-commit authorship
- amend
- squash commit resolution
- conflict resolution
- reset soft/mixed reconstruction
- stash save/restore

They should not be reconstructed from current filesystem content after the fact.

## Public Rewrite Event API

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

This is the right shape. Daemon command detection should normalize Git commands
into these events, and commit-note migration should flow through this one
entrypoint.

## Core Note-Shift Algorithm

For each `(source_commit, destination_commit)` mapping:

1. Batch-read source and destination notes with `notes_api::read_notes_batch`.
2. Resolve all unique commit SHAs to tree SHAs in one `git rev-parse` call.
3. Run one `git diff-tree --stdin -p -U0 -M --no-color -r` for all pairs.
4. Parse hunks and renames.
5. Shift attribution ranges that survive outside diff hunks.
6. Drop attribution ranges inside changed hunks.
7. Update `metadata.base_commit_sha` to the destination commit.
8. Batch-write destination notes with `notes_api::write_notes_batch`.

What is good:

- The algorithm is object-based.
- It avoids per-pair `diff-tree` spawns.
- It avoids per-note Git note reads/writes.
- It handles file renames.
- It does not pretend changed hunks are preserved.
- It can merge existing target notes when conflict-resolution checkpoints have
  already written destination attribution.

Known limitation:

- The algorithm is line-based. If a user expects semantically "same" content
  with changed spelling, punctuation, or formatting to retain attribution, this
  algorithm will not do that unless Git's diff keeps the line outside a hunk.
  That is intentional for now because fuzzy semantic matching would be a new
  heuristic layer.

## Non-Fast-Forward Rewrites

`RewriteEvent::NonFastForward` handles rebases, amended histories, restacks, and
other branch rewrites where an old tip is replaced by a new tip.

Implementation path:

1. Find merge base of `old_tip` and `new_tip`.
2. If `base == new_tip`, this is a backward reset; reconstruct the working log
   rather than migrating notes.
3. If `base == old_tip`, this is a fast-forward; no rewrite note migration is
   needed.
4. Otherwise, run `git range-diff` over the old and new ranges.
5. Parse range-diff output into `(old_commit, new_commit)` mappings.
6. Add merge-commit mappings where needed.
7. Shift notes for the mappings.

What is good:

- It uses Git's own range comparison rather than inventing a rebase matcher.
- It can represent reordered, modified, dropped, and squashed commits.
- Squashed/dropped source commits can map into a later destination commit.
- Once `old_tip` and `new_tip` are exact, mapping is based on immutable commits.

Sensitive boundary:

- `range-diff` is correct only after the daemon has exact old/new ranges.
- Guessing `old_tip`, `new_tip`, or `onto` from latest repo state is not valid.
- The daemon must not use stale rebase reflog history from unrelated prior
  commands.

## Rebase

Rebase handling is split between daemon detection and rewrite-note migration.

The daemon should provide:

- original branch tip
- final rebased branch tip
- onto/base hint when exactly known
- conflict-resolution checkpoint data if any

The rewrite core then:

- maps old commits to new commits with range-diff
- shifts preserved attribution
- merges resolution checkpoint attribution into destination notes

Conflict-resolution semantics:

- Preserved source-side lines retain source attribution.
- Preserved target-side lines retain target attribution.
- Keeping both sides should preserve both sets of source attribution.
- AI-resolved rewritten lines should get AI attribution from the resolution
  checkpoint.
- Human-resolved rewritten lines should get known-human attribution if known
  human checkpointed, otherwise remain unattributed.
- Uncheckpointed resolution content must not be attributed to old source commits
  just because it appears in the rebased commit.

What currently works according to tests:

- normal rebase preservation
- multi-commit rebase preservation
- pull --rebase preservation
- pull --rebase --autostash preservation
- several real-world conflict cases
- cold mid-rebase continue where the daemon starts after the failed raw rebase
  and sees the resolution checkpoints plus `rebase --continue`

What remains sensitive:

- `rebase --continue` can look like a fast-forward from onto to final tip.
  Correct handling needs pending original-head state from the failed rebase or
  a current exact in-progress command boundary.
- Reading `.git/rebase-merge` or `.git/rebase-apply` after a completed delayed
  command is mutable-state recovery and should be avoided.

## Cherry-Pick

`RewriteEvent::CherryPickComplete` maps source commits to newly created picked
commits.

Current pairing logic in `rewrite_cherry_pick.rs`:

- compute stable patch IDs for source and destination commits in batch
- pair identical patches first
- pair remaining unmatched commits positionally
- skipped sources produce no destination pair

What is good:

- Patch-id anchoring handles clean picks better than pure positional matching.
- The final note migration reuses the same batched shift core.
- Conflicted cherry-pick resolution can merge destination checkpoint notes.

What is not perfect:

- Symbolic source refs are mutable if resolved after delay.
- Source refs are exact only when they are immutable OIDs or resolved at a
  trusted command boundary.
- `--no-commit` changes the index/worktree, not commits. It must not synthesize
  committed attribution from a delayed current index unless the exact index/tree
  created by the command is captured as stable data.
- Positional gap-fill is acceptable only after exact source and destination
  sequences are known. It must not compensate for unknown command ownership.

## Reset

Backward reset reconstructs working-log attribution instead of writing notes.

Current implementation in `rewrite_reset.rs`:

1. List commits being undone: `new_tip..old_tip`.
2. Batch-read authorship notes for those commits.
3. Shift intermediate commit attributions into old-tip coordinate space.
4. Batch-read file contents from `old_tip` and `new_tip` trees.
5. Keep only files whose old-tip content differs from the reset target.
6. Write `INITIAL` working-log data under `new_tip`.
7. Preserve any checkpoint log appended after the reset by not clearing
   `checkpoints.jsonl`.
8. Delete the old working-log base directory when appropriate.

This is a first-principles fix for the reset race class. After `reset --soft` or
`reset --mixed`, the relevant uncommitted content is the old tip's tree content,
not the user's current worktree at daemon processing time.

What works:

- soft reset attribution preservation
- mixed reset attribution preservation
- multiple undone commits
- new files from undone commits
- mixed AI/human attribution
- reset from subdirectories

What remains sensitive:

- reset pathspec behavior is separate from branch-tip reset behavior
- hard reset should not reconstruct uncommitted attribution that no longer
  exists

## Squash Merge

Squash merge is a many-to-one rewrite from source commits into a single
destination commit.

Current implementation:

1. Receive `source_head`, `squash_commit`, and `onto`.
2. Find merge base of source and onto.
3. List source commits from base to source head.
4. Fetch/read all source notes.
5. Shift each source note into source-head coordinate space if needed.
6. Merge source logs.
7. Shift merged source log from source head to squash commit.
8. Merge with an existing squash-commit resolution note if one exists.
9. If there is a working log on `onto`, post the squash resolution working log
   with a transform that merges source attribution.

What is good:

- It handles many-to-one source attribution.
- It uses batched notes and diffs.
- It can merge preserved source attribution with conflict/resolution
  attribution on the squash commit.
- It supports an exact cold-start case when the command is
  `merge --squash <immutable-oid>`.

Known limitation:

- `merge --squash feature` is not exactly recoverable in a cold delayed command
  if `feature` can move before daemon processing. That must fail closed unless
  a cursor existed before the command.

## Stash

Stash is not a commit-note rewrite. It migrates working-log attribution across
stash create/apply/pop/drop.

Current implementation direction in `rewrite_stash.rs`:

- On stash create, save metadata keyed by stash SHA:
  - base commit
  - timestamp as metadata only
  - pathspecs
- Copy relevant working-log data into `.git/ai/stashes`.
- Clean stashed paths from the original working log.
- On apply/pop, restore copied working-log data to the target head.
- If the stash was created on a different base, reconstruct applied content
  using an isolated temporary index/worktree from stash object plus target head.
- Read the resulting content through a produced tree and
  `batch_read_paths_at_treeishes`, not from the user's live worktree.

This addresses the earlier race where stash restoration read files from the
current worktree after `stash pop/apply`.

Remaining caution:

- The isolated temp worktree must remain isolated from user hooks and daemon
  trace2 side effects.
- `stash@{N}` targets are mutable and must be resolved at a cursor-bounded
  command boundary.

## Revert

Revert is handled separately in `rewrite_revert.rs`.

Model:

- A revert can restore lines deleted by an earlier commit.
- Restored lines should recover the attribution they had when they previously
  existed.
- The implementation uses source/base commit data and note shifting rather than
  treating restored lines as human.

Tests cover:

- reverting an older deletion restores AI attribution
- line-number shifts before the revert do not lose restored attribution

## Performance Model

The rewrite core has been optimized away from the worst non-constant Git spawn
patterns:

- note reads use batch APIs
- note writes use batch APIs
- tree diffs use one `diff-tree --stdin` call for all pairs in a rewrite batch
- tree/blob reads use batch helpers where possible

There are still Git operations whose work scales with the number of commits or
files because Git itself must inspect those inputs:

- `range-diff` over old/new ranges
- `rev-list` for source ranges
- `log -p --stdin` plus `patch-id --stable` for cherry-pick pairing
- `diff-tree --stdin` over all mapped pairs

That scaling is acceptable if it is done with fixed batches rather than
spawning once per commit/file/note.

## Current Test Evidence

Representative coverage:

- `tests/integration/rewrite_ops_attribution.rs`
- `tests/integration/pull_rebase_ff.rs`
- `tests/integration/rebase_realworld.rs`
- `tests/integration/subdirs.rs`
- `tests/commit_tree_update_ref.rs`
- unit tests in `src/authorship/rewrite.rs`
- unit tests in `src/authorship/hunk_shift.rs`

Recent focused runs during this work showed `commit_tree_update_ref` and several
rewrite/cold focused tests passing. This should not be treated as full proof
until the broader daemon suite and full `task test` are green.

## What Looks Good

- The rewrite event API is small.
- The note shift algorithm is shared.
- The core uses immutable Git objects.
- Modified hunks are invalidated rather than guessed.
- Conflict resolution is represented through checkpoints, not hidden inference.
- Reset reconstruction no longer reads live worktree state.
- Stash shifted restore no longer reads live user worktree state.
- Batched Git calls significantly reduce process-spawn pressure.

## Remaining Risks

- Correctness still depends on daemon-side exact command facts.
- Symbolic refs after delay remain dangerous unless cursor-bounded.
- `--no-commit` cherry-pick should stay conservative unless exact index/tree
  state is available.
- Conflict resolution needs broader deterministic coverage for keep-ours,
  keep-theirs, keep-both, AI rewrite, human rewrite, and uncheckpointed rewrite.
- Any lingering path that reads mutable `.git/rebase-*` or live worktree state
  for a completed delayed command should be treated as suspect.

## Completion Requirements

Before calling rewrite ops complete:

1. All rewrite side effects use immutable commits/trees/notes or persisted
   working logs.
2. No mtime-based worktree snapshot remains in rewrite/post-commit handling.
3. Reset soft/mixed/hard/pathspec semantics are covered.
4. Stash push/apply/pop/drop/pathspec semantics are covered.
5. Squash merge handles immutable source OIDs and fails closed for unrecoverable
   symbolic cold sources.
6. Rebase and cherry-pick conflict resolution modes are covered.
7. Partial staging carryover is covered through TestRepo, not manual working-log
   writes.
8. Focused rewrite suites pass.
9. `task test` passes.
