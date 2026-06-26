# Pull Rebase Authorship Loss Analysis - 2026-06-21

## Incident

Repository: `/Users/svarlamov/projects/git-ai`

The observed flow was:

1. Local `main` was at `11afd537d`.
2. A local commit was created:
   `1e49c4dbf4d11c27c12ad041b5272556ecc03db5`
   (`update release workflow protections`).
3. `git push` was rejected because `origin/main` had advanced.
4. `git pull` fetched remote updates but failed with Git's divergent-branch strategy error.
5. `git pull --rebase` rebased the local commit onto `origin/main`, producing:
   `c1c7ae1006127e039d779c9692d760dd43434176`.
6. The authorship note remained on `1e49c4dbf...`, but no note was created for
   `c1c7ae100...`.

This was persistent: repeated `git ai show c1c7ae100...` did not recover the note.

## Durable Evidence

The original note existed and was readable:

```text
git notes --ref=ai list | rg '1e49|c1c7'
42973ad90d26e51d152d8df9d16ad7d92f76a041 1e49c4dbf4d11c27c12ad041b5272556ecc03db5
```

`git ai show 1e49c4dbf` showed authorship data for Codex session
`s_784e3644721859`; `git ai show c1c7ae100...` reported no authorship data.

The rebase mapping is exact:

```text
git range-diff --no-color --no-abbrev -s --creation-factor=100 \
  11afd537d..1e49c4dbf 5a3a97f65..c1c7ae100

1:  1e49c4dbf4d11c27c12ad041b5272556ecc03db5 = 1:  c1c7ae1006127e039d779c9692d760dd43434176 update release workflow protections
```

The commit relationship is non-fast-forward from the old local commit to the
rebased commit:

```text
git merge-base 1e49c4dbf c1c7ae100
11afd537d...

git merge-base --is-ancestor 1e49c4dbf c1c7ae100
# exit 1

git merge-base --is-ancestor 5a3a97f65 c1c7ae100
# exit 0
```

The relevant reflog shape was:

```text
HEAD:
11afd537d -> 1e49c4dbf  commit: update release workflow protections
1e49c4dbf -> 5a3a97f65  pull --rebase (start): checkout 5a3a97f65...
5a3a97f65 -> c1c7ae100  pull --rebase (pick): update release workflow protections
c1c7ae100 -> c1c7ae100  pull --rebase (finish): returning to refs/heads/main

refs/heads/main:
11afd537d -> 1e49c4dbf  commit: update release workflow protections
1e49c4dbf -> c1c7ae100  pull --rebase (finish): refs/heads/main onto 5a3a97f65...

refs/remotes/origin/main:
11afd537d -> 5a3a97f65  pull: fast-forward
```

The notes reflog showed a note write for `1e49c4dbf...`, followed by a notes
merge from the pull, and then an empty fast-import. There was no later write for
`c1c7ae100...`. That means the target note was never written, not written and
then deleted.

The repo's `.git/ai/rewrite_log` had no entries for `1e49c4dbf...`,
`c1c7ae100...`, `5a3a97f65...`, or `11afd537d...`.

## Ruled Out

- Missing source note: ruled out. The note exists and deserializes through
  `git ai show 1e49c4dbf`.
- Missing or ambiguous rebase mapping: ruled out. `range-diff` maps the old and
  new commits exactly.
- HTTP/notes-db backend inconsistency: ruled out for this repo. The config uses
  Git notes, and the notes DB did not contain either commit.
- Later cleanup deleting the target note: unlikely. The notes reflog does not
  show a target-note write for `c1c7ae100...`.
- Daemon totally missing the successful pull: unlikely. The notes merge after
  the pull proves the successful pull command was processed far enough to run
  pull side effects.
- Missing user-facing daemon log line for pull/rebase: not evidence by itself.
  The current daemon info log for `git write op completed` is not emitted for
  every tracked write command.

## Root Cause

The failure happened because the successful `pull --rebase` did not supply
`detect_and_handle_non_ff_rewrites` with a non-fast-forward old/new pair
`1e49c4dbf... -> c1c7ae100...`.

The concrete mechanism is a cold reflog cursor plus late async offset capture
for the multi-row `pull --rebase` reflog span:

1. Trace2 command frames are received by the daemon after Git has already
   written some or all reflog rows.
2. `capture_reflog_start_offsets_for_worktree` snapshots current reflog byte
   lengths asynchronously (`src/daemon/ref_cursor.rs:1879`).
3. The preceding commit can still get an authorship note while leaving the HEAD
   cursor cold. If the commit's async start offset is captured at EOF after the
   commit row, `command_start_offset_is_authoritative` returns false
   (`src/daemon/ref_cursor.rs:1629`). Enrichment can still find the commit row by
   scanning, but `consume_entry` only marks it consumed and then compacts from
   the current cursor floor (`src/daemon/ref_cursor.rs:1678`); with no usable
   floor, older unconsumed reflog rows prevent the cursor from becoming healthy.
4. On the later `pull --rebase`, a cold cursor can use the captured offset as a
   hard starting floor (`src/daemon/ref_cursor.rs:190`).
5. The current cold-start clamp only knows how to recognize `commit` entries
   (`src/daemon/ref_cursor.rs:288`). It does not know how to clamp back to the
   beginning of a `pull --rebase` span.
6. If the daemon starts at or after the first pull-rebase HEAD row, enrichment
   can miss `HEAD 1e49c4dbf -> 5a3a97f65`.
7. The daemon may then expose only `HEAD 5a3a97f65 -> c1c7ae100`, which is a
   fast-forward, or expose no useful branch/HEAD rewrite pair at all. If the
   branch cursor also seeds after the `refs/heads/main` finish row, it misses the
   direct branch rewrite `1e49c4dbf -> c1c7ae100`.
8. `detect_and_handle_non_ff_rewrites` filters to branch changes, falls back to
   HEAD changes only when branch changes are empty, and skips any collapsed pair
   where `old_tip` is an ancestor of `new_tip`
   (`src/daemon.rs:3987`, `src/daemon.rs:4018`, `src/daemon.rs:4098`).
9. Since rewrite detection never calls
   `handle_non_fast_forward_rewrite(repo, 1e49c4dbf..., c1c7ae100..., ...)`, the
   note migration never runs.

This explains why the source note is intact, why the target note is absent, why
the pull notes merge still happened, and why the failure was persistent.

Confidence: high that the miss is before rewrite-note writing; high that the
mechanism is cold/late cursor seeding for the pull-rebase span. The exact
`NormalizedCommand.ref_changes` for the original incident was not persisted, so
that internal value cannot be directly recovered.

## Relevant Code Paths

- `src/daemon/ref_cursor.rs:190`:
  command-start reflog offsets initialize or hint the cursor.
- `src/daemon/ref_cursor.rs:248`:
  cold-start seed clamping can move a late offset back to a matching own entry.
- `src/daemon/ref_cursor.rs:288`:
  cold-start match specs currently exist only for commit/amend.
- `src/daemon/ref_cursor.rs:1051`:
  `pull --rebase` HEAD spans are consumed from a start row when one is visible.
- `src/daemon/ref_cursor.rs:1108`:
  `find_pull_start_entry` scans from the cursor's reflog start offset.
- `src/daemon/ref_cursor.rs:1629`:
  cold command-start offsets are treated as authoritative only when records
  exist after the offset.
- `src/daemon/ref_cursor.rs:1678`:
  consuming a found row marks it consumed and compacts from the existing cursor
  floor; it does not by itself establish a healthy cursor if older rows block
  compaction.
- `src/daemon/ref_cursor.rs:2539`:
  no-op reflog rows are filtered, so the no-op rebase finish row is not a
  recoverable transition.
- `src/daemon.rs:3987`:
  non-fast-forward rewrite detection starts from enriched `cmd.ref_changes`.
- `src/daemon.rs:4098`:
  fast-forward pairs are skipped.
- `src/authorship/rewrite.rs:60`:
  once called with the correct old/new pair, rewrite handling derives
  range-diff mappings and shifts notes.
- `src/authorship/rewrite.rs:337`:
  a missing source note would skip a mapping, but the source note in this
  incident exists.

## Subagent Review

Three read-only code reviews converged on the same boundary:

- Cursor/enrichment review: confirmed the commit note can be written while the
  reflog cursor remains cold, and that `pull --rebase` has no cold-start clamp
  for its start/pick/finish span. This is the most likely direct root cause.
- Rewrite/notes review: found no plausible silent failure once correct
  `ref_changes` and the source note are present. `handle_non_fast_forward_rewrite`
  would range-diff, fetch missing source notes, and write target notes or return
  an error. Side-effect errors would be logged.
- Test harness review: confirmed `TestRepo::git()` does not literally block
  after every command. It marks tracked commands with a test-sync session and
  records expected completions; the actual wait happens at explicit sync/read
  boundaries. `pull` is tracked, but the harness still runs in a fresh temp repo
  with cleaner cursor and reflog state than the real incident.

No subagent found a more likely root cause outside ref-change enrichment.

## Why A Simple Passing Test May Not Reproduce

A plain `TestRepo` test can run the same user-level commands and still pass if
the daemon captures reflog offsets early enough or already has healthy cursors
for the HEAD and branch reflogs.

The harness also adds test-sync config to tracked commands. In
`tests/integration/repos/test_repo.rs`, `pull` is a tracked command for test sync
(`src/daemon/test_sync.rs:16`), and `git()` records expected completion sessions
for tracked invocations (`tests/integration/repos/test_repo.rs:2695`,
`tests/integration/repos/test_repo.rs:2756`). The actual wait happens through
`sync_daemon_force` (`tests/integration/repos/test_repo.rs:2226`), typically
from read/assertion helpers such as `read_authorship_note`
(`tests/integration/repos/test_repo.rs:3033`) or commit helpers
(`tests/integration/repos/test_repo.rs:3123`). That makes normal tests more
deterministic than the user's shell flow without guaranteeing the same cursor
state.

This does not make TestRepo invalid. It means a simple normal-flow red test must
either naturally reproduce the cold/late cursor state or set up the same state
using normal git-ai operations only. If the simple test cannot reproduce, that
is useful evidence that the bug depends on a timing/cursor-state precondition
not automatically present in a fresh harness repo.

## Remaining Questions

- What exact cursor state existed for HEAD and `refs/heads/main` before the
  successful `pull --rebase` command in the real daemon?
- Did the failed plain `git pull` leave a cold/late branch cursor or only update
  the remote-tracking cursor?
- Did the successful `pull --rebase` produce `cmd.ref_changes` containing only
  the fast-forward pick row, only the branch finish row, no useful changes, or a
  different collapsed set?

The durable repo state strongly narrows the failure to enrichment/rewrite
detection, but those exact in-memory command details were not persisted.

## Remediation Direction

The likely fix is to make pull-rebase/rebase span recovery robust under cold and
late ingress offsets:

- Add a command-specific cold-start match spec for `pull --rebase`/rebase spans,
  not just commit entries.
- Ensure span recovery can clamp to and consume the first non-noop HEAD row
  (`old local tip -> onto`) when the ingress offset lands inside the span.
- Ensure branch finish recovery produces or preserves the collapsed
  `old local tip -> rebased tip` non-fast-forward pair.
- Add a faithful e2e red test that uses the normal daemon and normal TestRepo
  flow, keeping any special setup to ordinary git/git-ai operations. If that
  cannot reproduce the failure, document the remaining timing/state delta rather
  than forcing it with custom trace2 injection.

## Hardening Implemented

See `docs/pull-rebase-hardening-worklog-2026-06-21.md` for the running log.

The implemented fix keeps the daemon's trace2 ingestion asynchronous and does
not change production operation ordering. It makes cold-start reflog seed
clamping span-aware for `pull --rebase` and `rebase`:

- late offsets inside a pull/rebase span are clamped back to the span start;
- common-ref offsets inside the branch finish row are clamped back to that row;
- true command-start boundaries are not rewound into older completed spans.

The deterministic proof is the cursor-level red test
`cold_pull_rebase_late_ingress_offset_still_recovers_start_and_branch_finish`,
which failed before the fix with only the fast-forward pick row and passes after
the fix with the full non-fast-forward pair available for rewrite detection.
