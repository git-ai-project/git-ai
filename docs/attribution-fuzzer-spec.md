# Attribution Fuzzer Spec

Status: current design and recommended direction for the attribution fuzzer on
`feat/attr-fuzzer-v2`.

This document covers the fuzzer only. Rewrite-operation semantics are documented
in `docs/rewrite-ops-spec.md`; daemon trace2 ingestion is documented in
`docs/daemon-trace2-ingestion-spec.md`.

## Purpose

The fuzzer should pressure Git AI's attribution invariants across long,
pathological sequences of edits and Git operations. It should find cases humans
would not naturally write by hand, then produce enough evidence to turn each
failure into a deterministic TestRepo regression test.

The fuzzer is not a replacement for deterministic tests. It is a discovery tool.
Every real fuzzer failure should become:

1. a saved seed and operation log
2. a minimized deterministic TestRepo test
3. a failing assertion before the fix
4. a fix for the underlying class of issue
5. retained fuzzer coverage if the seed is cheap enough

## Current Implementation

Files:

- `tests/integration/fuzzer/model.rs`
- `tests/integration/fuzzer/operations.rs`
- `tests/integration/fuzzer/engine.rs`
- `tests/integration/fuzzer/helpers.rs`
- `tests/integration/fuzzer/mod.rs`

Current model:

- A file line is represented by a unique Unicode character.
- `AttrRegistry` remembers the checkpoint-time attribution for each character.
- `FileModel` stores current line order and expected attribution.
- The fuzzer writes the model to disk, performs Git/Git AI operations through
  `TestRepo`, then asserts `git-ai blame` against the model.

Current attribution states:

- `Ai`
- `KnownHuman`
- `Untracked`

Current operations:

- random edit
- checkpoint as AI
- checkpoint as known human
- checkpoint as untracked legacy human
- commit
- amend
- rebase
- cherry-pick

Current configurations:

- standard fixed seeds
- rewrite-heavy fixed seeds
- one default random test
- ignored marathon/chaos tests

## What Is Good

The current fuzzer has useful properties:

- One unique character per line makes line identity clear.
- The model is independent of Git AI notes; it does not bless implementation
  output as expected behavior.
- Assertion failures include the seed, operation log, blame output, and model
  dump.
- It performs real Git operations through `TestRepo`.
- Fixed seeds provide stable regression pressure.
- Rewrite-heavy and marathon modes are the right direction for exercising long
  sequences.

## Current Weaknesses

The current fuzzer is not yet a complete proof harness.

### Random seed reproducibility

The default `fuzz_random` test currently derives a seed from system time. If
that test can fail in CI, it must print the seed every time or move to
ignored/nightly. A non-reproducible random CI failure is not acceptable.

### AI vs non-AI assertion is too narrow

The current blame assertion mainly checks whether the author is AI. It does not
fully distinguish known-human attribution from untracked attribution in every
path. For Git AI invariants, that distinction matters.

### Operation coverage is too narrow

The fuzzer mostly operates on one file and simple line identities. It does not
yet deeply model:

- partial staging
- multi-file changes
- file rename/delete/recreate
- stash
- reset
- squash merge
- pull --rebase
- pull --rebase --autostash
- branch lifecycle
- reflog pruning/truncation
- daemon restart
- delayed trace replay
- cold setup with trace2 disabled
- symlink/canonical path variants

### Conflict handling is under-modeled

Current rebase/cherry-pick operations may abort on conflict rather than modeling
rich resolution behavior. The most important attribution bugs have been in
conflict resolution, so this is a major gap.

## Desired Invariants

The fuzzer should eventually assert all of these:

1. AI lines that survive unchanged through Git rewrites remain AI.
2. Known-human lines that survive unchanged remain known human.
3. Untracked lines remain unattributed unless later checkpointed.
4. Deleted lines do not leave stale attribution ranges.
5. Renamed files preserve surviving attribution under the new path.
6. Partial staging attributes only the committed content.
7. Unstaged attribution carries forward after partial commits.
8. Reset soft/mixed reconstructs working-log attribution from undone commits.
9. Reset hard discards attribution for discarded worktree/index content.
10. Stash push/apply/pop preserves uncommitted attribution without reading the
    user's live worktree after the fact.
11. Clean rebase preserves unchanged source attribution.
12. Clean cherry-pick preserves unchanged source attribution.
13. Squash merge preserves source attribution for exact immutable source commits.
14. Conflict keep-ours preserves target-side attribution.
15. Conflict keep-theirs preserves source-side attribution.
16. Conflict keep-both preserves attribution for both preserved sides.
17. AI conflict rewrite gets AI attribution from the resolution checkpoint.
18. Known-human conflict rewrite gets known-human attribution from the resolution
    checkpoint.
19. Uncheckpointed conflict rewrite remains unattributed.
20. Cold-start no-cursor commands fail closed rather than guessing.
21. Reflog truncation/pruning clears invalid cursors and does not panic.
22. Symlink/canonical path variants resolve to the same family/cursor semantics.
23. No operation requires hidden daemon sync except explicit test assertion sync.
24. The daemon never deadlocks on partial trace2 roots, incomplete reflog lines,
    socket close ordering, or child trace traffic.

## Desired Operations

The fuzzer should grow operation families for:

- single-file insert/replace/delete
- multi-file insert/replace/delete
- file rename
- file delete and recreate
- AI checkpoint
- known-human checkpoint
- untracked legacy-human checkpoint
- no checkpoint at all
- full commit
- partial stage and commit
- amend
- reset soft
- reset mixed
- reset hard
- reset pathspec
- stash push
- stash push with pathspec
- stash apply
- stash pop
- stash drop
- clean rebase
- conflicted rebase
- clean cherry-pick
- conflicted cherry-pick
- merge --squash with immutable source OID
- pull --rebase
- pull --rebase --autostash
- branch create/delete/recreate
- branch rename/copy
- daemon restart
- delayed trace replay
- cold setup with trace2 disabled followed by traced commands

## Conflict Resolution Modes

For conflicted rebase and cherry-pick, the fuzzer should explicitly generate
these resolution modes:

- keep ours
- keep theirs
- keep both ours then theirs
- keep both theirs then ours
- AI rewrite entire conflict region
- known-human rewrite entire conflict region
- uncheckpointed rewrite entire conflict region
- delete both sides
- add unrelated new lines around preserved lines

For each mode, the expected attribution must be derived from first principles:

- preserved old lines keep their original attribution
- new checkpointed lines get the checkpoint attribution
- new uncheckpointed lines remain unattributed
- old lines removed from the final file disappear from assertions

## Model Requirements

The current unique-character-per-line model is a good base, but it should
eventually represent:

- multiple files
- line content independent from line identity
- known-human vs untracked distinction in assertions
- branch-local model state
- index/staged model state for partial staging
- working-tree model state
- committed-history model state
- stash stack model state
- daemon known-cursor/cold-start state

The fuzzer should not read Git AI notes to update expected attribution. Notes
are the implementation under test.

## Reproducibility Requirements

Every fuzzer run must make failure reproduction straightforward:

- print seed for every random run
- include operation count
- include operation log
- include current branch
- include relevant commit SHAs
- include expected model dump
- include actual blame output
- include exact failing command

Random/non-deterministic tests should not run in default CI unless their seed is
always emitted and failure replay is straightforward.

## Deterministic Regression Rule

When the fuzzer finds a failure:

1. Minimize the operation log.
2. Write a deterministic TestRepo test that reproduces the same failure.
3. Assert line-level content and attribution after every commit.
4. Avoid manual working-log writes.
5. Use explicit checkpoint calls when the exact checkpoint flow matters.
6. Make the test fail before changing production logic.
7. Fix the root cause.
8. Keep the minimized test permanently.

## Current Test Evidence

Current fuzzer tests live in `tests/integration/fuzzer/mod.rs`:

- `fuzz_standard_seed_0`
- `fuzz_standard_seed_1`
- `fuzz_standard_seed_42`
- `fuzz_standard_seed_99`
- `fuzz_standard_seed_1337`
- rewrite-heavy fixed seeds
- one default random seed
- ignored marathon tests

These are useful pressure, but they do not prove all attribution invariants.
They should be treated as an early fuzzer, not the final proof harness.

## Recommended Next Steps

1. Print the seed for `fuzz_random` or mark it ignored.
2. Extend assertions to distinguish AI, known human, and untracked.
3. Add multi-file model state.
4. Add staged/index model state.
5. Add conflict-resolution operations.
6. Add stash/reset/squash/pull-rebase operations.
7. Add cold-start and delayed-trace operation modes.
8. Add symlink/canonical path variants.
9. Convert every discovered failure into a deterministic TestRepo regression.

## Completion Requirements

Before treating the fuzzer as a serious proof layer:

1. Default fuzzer runs must be reproducible.
2. The model must assert all three attribution classes.
3. The model must cover files, branches, index, working tree, and stash state.
4. Conflict resolution modes must be represented.
5. Cold-start and delayed ingestion cases must be represented.
6. Any failure must print enough information to replay locally.
7. Every known historical race must have a deterministic TestRepo test outside
   the fuzzer.
