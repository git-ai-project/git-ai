# Attribution Fuzzer Spec

Status: authoritative spec for the attribution fuzzer. Companion docs:

- `docs/rewrite-ops-spec.md` — the semantics under test.
- `docs/daemon-trace2-ingestion-spec.md` — the ownership model under test.

## Purpose

The fuzzer pressures git-ai's attribution invariants across long,
pathological sequences of edits, checkpoints, and git operations — sequences
no one would write by hand. It is a *discovery tool*, not the proof layer:
the deterministic suite proves; the fuzzer finds what the suite is missing.

Every real fuzzer failure becomes:

1. a reproducible seed + operation log (printed on failure),
2. a minimized deterministic TestRepo regression that fails before the fix,
3. a root-cause fix,
4. a permanent regression test (and the seed retained if cheap).

## Model

The fuzzer maintains its own expected-attribution model, fully independent of
git-ai's output. It never reads authorship notes or blame to update
expectations — notes and blame are the implementation under test.

- **Line identity**: each created line is a unique Unicode character
  (allocated from U+4E00). Content equality is identity; survival across
  operations is unambiguous.
- **AttrRegistry**: permanent map from line character → attribution class at
  checkpoint time. Three classes, matching git-ai's model:
  `Ai` (mock_ai checkpoint), `KnownHuman` (mock_known_human checkpoint),
  `Untracked` (legacy `human` checkpoint or never checkpointed).
- **FileModel** per file: ordered line characters + per-line resolved class.
  The model tracks: working-tree state, what has been checkpointed (and as
  what class), and what is committed.
- **Branch/stash state**: branch-local committed models and a stash stack of
  saved working-tree deltas, so rebase/cherry-pick/stash operations have
  first-principles expected outcomes.

Expected attribution under each operation is derived from the invariants in
`docs/rewrite-ops-spec.md` (I1–I4), notably: surviving lines keep their
class; lines rewritten in conflict hunks take the resolution checkpoint's
class or become untracked; uncommitted work follows reset/stash per spec.

## Assertions

After every commit (and after every rewrite operation), the fuzzer asserts
`git-ai blame` against the model **for all three classes**, not just
AI/non-AI:

- `Ai` lines must blame to an AI author;
- `KnownHuman` lines must blame to a human author *with* known-human
  attestation;
- `Untracked` lines must blame to a human author *without* attestation.

The known-human/untracked distinction is observable via blame's attestation
hashes (`h_`-prefixed known-human attestations vs unattested); the fuzzer
must use an output mode that exposes it (`--show-prompt`-style hash names)
rather than display-name parsing alone where possible.

Failure output must include: seed, operation count, full operation log,
current branch and relevant commit SHAs, expected model dump, actual blame
output, and the exact failing command — enough to replay locally with zero
archaeology.

## Reproducibility rules

- Every run prints its seed (including `fuzz_random`, which derives one from
  time). A CI failure that cannot be replayed is a spec violation.
- Fixed-seed tests (`fuzz_standard_seed_*`, `fuzz_rewrite_heavy_seed_*`) run
  in default CI as stable regression pressure.
- Marathon/chaos configurations (150+ ops) are `#[ignore]`d, run nightly or
  on demand.
- The op-log alone must be sufficient to hand-write the deterministic
  minimization: ops are recorded with their concrete arguments (file, lines,
  resolution mode), not just names.

## Operation families

Tier 1 (implemented, must remain):

- single-file insert/replace/delete edits
- checkpoint as AI / known-human / untracked; edits with no checkpoint
- commit, amend
- clean rebase, clean cherry-pick

Tier 2 (required for the fuzzer to be considered comprehensive):

- multi-file edits; file rename; file delete + recreate
- partial staging (`git add <file>` subsets, commit, carryover assertion)
- reset soft / mixed / hard
- stash push / apply / pop (same-base and shifted-base)
- conflicted rebase and cherry-pick with explicit resolution modes:
  keep-ours, keep-theirs, keep-both (both orders), AI-rewrite,
  known-human-rewrite, uncheckpointed-rewrite, delete-both
- squash merge with immutable source OID
- pull --rebase (with and without --autostash)
- branch create/delete/recreate

Tier 3 (environmental hostility):

- daemon restart between operations
- reflog prune/truncation between operations
- cold-start sequences (operations before the daemon attaches)
- symlinked/canonicalized repo paths

For each conflicted-rewrite mode, the expected attribution is computed from
first principles: preserved lines keep their class; checkpointed rewrites get
the checkpoint class; uncheckpointed rewrites are untracked; deleted lines
vanish from expectations.

## Engine rules

- Single seeded `StdRng`; no other entropy sources.
- Operation weights per configuration (standard / rewrite-heavy / chaos).
- An operation that cannot apply in the current state (e.g. stash pop with an
  empty stack) is skipped deterministically, and the skip is logged.
- The fuzzer uses only public surfaces: real file writes, `git-ai checkpoint
  mock_*`, real git commands through TestRepo, and explicit daemon sync only
  immediately before assertions (mirroring the production no-hidden-sync
  rule).
- No manual working-log writes; no note reads for expectations.

## Relationship to the deterministic suite

The fuzzer never substitutes for deterministic coverage. Every invariant in
the rewrite-ops spec has a deterministic test independent of the fuzzer;
the fuzzer explores their composition. When the fuzzer finds a failure, the
deterministic minimization is the artifact that prevents regression — the
seed is kept only as cheap extra pressure.
