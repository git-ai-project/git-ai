# Pull Rebase Hardening Worklog - 2026-06-21

## Goal

Prove and harden the `pull --rebase` authorship-loss failure without changing
production operational semantics:

- keep trace2 as the source of git command events;
- keep daemon processing asynchronous;
- do not replace trace2 frames or inject fake git operations in e2e coverage;
- use TestRepo normal daemon flow where possible;
- keep deterministic unit coverage for the exact ref-cursor failure shape.

## Current Hypothesis Under Test

The daemon can write the source commit note while the reflog cursor remains
cold. A later `pull --rebase` command can then receive async reflog-start
offsets after the rebase start row or branch finish row. Current cold-start
clamping recognizes commits but not pull-rebase spans, so enrichment can miss the
non-fast-forward pair `old local commit -> rebased commit`.

## Test Plan

1. Add a red unit test in `src/daemon/ref_cursor.rs` that forces:
   - cold cursor;
   - `pull --rebase` HEAD reflog containing start, pick, and no-op finish;
   - branch reflog containing the direct finish move;
   - captured offsets after the HEAD start row and at branch EOF.
2. Add or adjust a plain TestRepo repro that runs:
   - source AI commit with confirmed note;
   - rejected push;
   - failed plain pull;
   - successful `pull --rebase`;
   - target note assertion.
3. Add a cold-repo TestRepo scenario initialized with trace2 disabled, then
   start the normal daemon and run pull-rebase under delayed/pressured ingest to
   look for nondeterministic reproduction without custom trace2 frames.
4. Fix cursor enrichment so pull-rebase spans recover from late cold offsets.
5. Run focused tests repeatedly enough to validate both deterministic and
   pressure paths.

## Findings

- Existing `TestRepo::git()` does not literally wait after every command. It
  records expected daemon sessions for tracked commands; explicit sync/read
  boundaries wait for those sessions.
- The deterministic code-level failure shape is independent of note writing:
  the source note can exist, and migration still fails if `cmd.ref_changes`
  lacks the non-fast-forward old/new pair.
- A purely normal TestRepo flow proves the user-visible command sequence and can
  stress it, but it does not deterministically force the late-offset/cold-cursor
  timing. The deterministic red proof is therefore at the ref-cursor enrichment
  boundary; adding a test-only trace timing hook would be more invasive and was
  not needed for the fix.
- Added `cold_pull_rebase_late_ingress_offset_still_recovers_start_and_branch_finish`
  in `src/daemon/ref_cursor.rs`. Before the fix it failed with only
  `HEAD C -> D`, exactly proving the missing non-fast-forward pair. After the
  fix it recovers `HEAD B -> C`, `HEAD C -> D`, and
  `refs/heads/main B -> D`.
- Added the symmetric
  `cold_rebase_late_ingress_offset_still_recovers_start_and_branch_finish`
  coverage for plain `git rebase`.
- Added true-boundary guard tests for pull-rebase and rebase to prove the new
  cold span clamp does not rewind a legitimate command-start offset into an
  older completed span.
- Added
  `test_cold_repo_first_traced_pull_rebase_preserves_rebased_ai_authorship` in
  `tests/integration/cold_trace2_repo.rs`: repo and remote are initialized with
  trace2 disabled, then the daemon starts and runs the exact rejected push,
  failed pull, successful pull-rebase flow.
- Added ignored stress coverage
  `stress_cold_repo_first_traced_pull_rebase_preserves_rebased_ai_authorship`;
  one explicit run completed 24 cold pull-rebase attempts across 8 concurrent
  workers without reproducing the note loss after the fix.

## Fix Direction

- Keep async trace2 ingestion unchanged.
- Add span-aware cold-start clamping for pull and rebase reflog spans.
- Avoid moving a true pre-command offset backward into old history by refusing
  to clamp if a newer matching start row exists after the captured offset.
- For common refs, clamp only when the captured offset is inside the matching
  branch reflog row and there is no newer matching row after the offset.
- If a newer matching start row exists after the captured offset, treat the
  offset as a real command-start boundary and do not clamp backward.

## Verification Log

- `task test TEST_FILTER=cold_pull_rebase_late_ingress_offset_still_recovers_start_and_branch_finish CARGO_TEST_ARGS="--lib" NO_CAPTURE=true`
  - failed before the fix with only `HEAD C -> D`;
  - passed after the fix.
- `task test TEST_FILTER=cold_start_late_ingress_offset_does_not_skip_commit_on_uninitialized_head_cursor CARGO_TEST_ARGS="--lib"`:
  passed.
- `task test TEST_FILTER=commit_reflog_boundary_skips_untraced_duplicate_message CARGO_TEST_ARGS="--lib"`:
  passed.
- `task test TEST_FILTER=pull_rebase_span_starts_at_start_entry_when_expected_state_matches_pick CARGO_TEST_ARGS="--lib"`:
  passed.
- `task test TEST_FILTER=rebase_span_stops_at_new_rebase_start_before_finish CARGO_TEST_ARGS="--lib"`:
  passed.
- `task test TEST_FILTER=test_rejected_push_failed_pull_then_pull_rebase_preserves_committed_ai_authorship NO_CAPTURE=true`:
  passed.
- `task test TEST_FILTER=test_cold_repo_first_traced_pull_rebase_preserves_rebased_ai_authorship NO_CAPTURE=true`:
  passed.
- `task test TEST_FILTER=stress_cold_repo_first_traced_pull_rebase_preserves_rebased_ai_authorship EXTRA_TEST_BINARY_ARGS="--ignored" NO_CAPTURE=true`:
  passed.
- `task test TEST_FILTER=cold_pull_rebase_true_boundary_does_not_replay_older_pull_span CARGO_TEST_ARGS="--lib"`:
  passed.
- `task test TEST_FILTER=cold_rebase_true_boundary_does_not_replay_older_rebase_span CARGO_TEST_ARGS="--lib"`:
  passed.
- `task test TEST_FILTER=cold_ CARGO_TEST_ARGS="--lib"`:
  passed 9 cold ref-cursor tests.
- `task test TEST_FILTER=daemon::ref_cursor::tests CARGO_TEST_ARGS="--lib"`:
  passed all 40 ref-cursor unit tests.
- `task test CARGO_TEST_ARGS="--lib"`:
  passed all 1840 lib tests.
- `task test TEST_FILTER=cold_trace2_repo`:
  passed 32 non-ignored cold trace2 integration tests; ignored only the new
  stress test unless explicitly requested.
- `task test TEST_FILTER=pull_rebase_ff`:
  interrupted after several minutes while
  `test_fast_forward_pull_without_local_changes` was still running; all emitted
  pull/rebase tests before the interruption had passed. This needs a narrower
  follow-up if we want a full `pull_rebase_ff` sweep in this branch.
- `task test TEST_FILTER=test_fast_forward_pull_without_local_changes NO_CAPTURE=true`:
  passed both generated variants quickly; the broad-filter interruption does
  not appear to be a deterministic failure in that test.
- Serial retry via `EXTRA_TEST_BINARY_ARGS="--test-threads 1"` is not supported
  by the task wrapper because it already supplies `--test-threads 10`.
- `task build`:
  passed.
- `task lint`:
  passed.
- `task test`:
  completed the full test run. The pull/rebase and rebase-heavy integration
  tests, including `pull_rebase_ff`, passed in this run. The suite failed only
  in `graphite::test_gt_navigation_preserves_attribution` with:
  `Failed to execute gt ["checkout", "nav-2"]: No such file or directory (os error 2)`.
  That failure is outside the ref-cursor/pull-rebase path changed here.
