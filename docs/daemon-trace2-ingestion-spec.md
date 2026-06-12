# Daemon Trace2 Ingestion Spec and Postmortem

Status: current design notes and postmortem for daemon trace2 ingestion work on
`feat/attr-fuzzer-v2`.

This document covers the daemon trace2 path, ref cursor model, cold-start
behavior, and failed approaches explored during the rewrite-ops work. Rewrite
semantics are documented in `docs/rewrite-ops-spec.md`; fuzzer design is
documented in `docs/attribution-fuzzer-spec.md`.

## Summary

The daemon trace2 ingestion problem is exact command ownership. The daemon sees
trace2 roots asynchronously, while Git mutates refs synchronously inside the Git
process. For attribution to be correct, the daemon must know which ref/log
entries belong to which command.

The correct boundary is a pre-command ref cursor:

- If the daemon had a cursor before a ref-moving command, it can consume reflog
  entries appended after that cursor.
- If the command itself contains immutable OIDs sufficient to identify the
  operation, those OIDs are exact for that operation.
- If neither is true, stock Git trace2 does not provide enough information to
  recover the command's ref transition after delay.
- The correct behavior for that command is to fail closed, then observe the
  current reflog end as a baseline for future commands.

This is stricter than some earlier approaches, but it is the only model that is
not heuristic.

## First Principles

### Trace2 is event data, not a ref transaction log

Stock Git trace2 records command lifecycle events such as start, def_repo,
cmd_name, exit, and atexit. It does not reliably include the created commit SHA
or complete ref update OIDs for normal `git commit` and many other commands.

Therefore a delayed trace2 root that says "git commit ran" is not enough to
identify the exact reflog line if the daemon did not already know where the
reflog was before the command.

### Reflog order is useful; timestamps are not proof

Raw reflog entries include timestamps:

```text
<old> <new> <author> <timestamp> <tz>\t<message>
```

But reflog timestamps are seconds-resolution and are not causally linked to a
trace2 root. Multiple commands can share a timestamp. Commit messages also
collide. Timestamp/message matching is therefore not exact.

The useful reflog facts are:

- append order
- byte offsets
- complete line boundaries
- `old` and `new` OIDs
- an anchor proving a saved offset still belongs to the same reflog generation

### The live worktree is not command history

Daemon side effects must not reconstruct past Git state by reading the current
worktree. That was the original post-commit carryover race and also appeared in
stash handling. The live worktree can change after Git exits and before daemon
processing.

The only valid live-worktree snapshot point is an explicit checkpoint.

## Current Data Path

Trace2 ingestion currently flows through these layers:

1. The trace2 socket listener accepts JSON frames.
2. `prepare_trace_payload_for_ingest` filters definitely read-only roots.
3. Mutating roots are enqueued with a sequence number.
4. `TraceNormalizer` groups frames by root sid.
5. Terminal root events produce a `NormalizedCommand`.
6. The coordinator sequences commands per repository family.
7. `RefCursor::enrich_command` consumes cursor-bounded reflog entries and fills
   `cmd.ref_changes`.
8. The side-effect layer applies post-commit, rewrite, stash, pull/push, and
   other behavior.

The important architectural split:

- normalizer parses trace2/argv facts
- family actor owns ordered repo-family state
- ref cursor belongs to the family actor
- side effects run after command enrichment

The normalizer should not read mutable repo state to synthesize missing command
facts.

## `NormalizedCommand`

Current command data includes:

- scope/family
- worktree
- root sid
- raw argv
- primary command
- invoked command/args
- observed child commands
- exit code
- trace start/finish timestamps
- optional `reflog_start_offsets`
- stash target OID
- cherry-pick source OIDs
- revert source OIDs
- `ref_changes`
- confidence

The risky field is `reflog_start_offsets`. It is exact only if it came from a
trusted command-start boundary. It is not exact if captured by the daemon after
the daemon asynchronously noticed a trace2 frame.

## Ref Cursor Model

`RefCursor` stores:

- per-ref byte offsets
- per-ref anchors
- consumed offsets and anchors
- in-memory stash stack
- pending cherry-pick source OIDs

Cursor keys distinguish:

- worktree-specific `HEAD` reflogs
- common-dir refs such as `refs/heads/main`
- `refs/stash`

The cursor must handle:

- missing reflog files
- partial trailing reflog lines
- reflog truncation/pruning
- branch delete/recreate
- common refs and per-worktree HEAD refs
- stale consumed offsets

Important behavior:

- `read_reflog_records` ignores incomplete trailing lines.
- `read_reflog_record_ending_at` validates a saved offset ends at a newline.
- A saved offset is reused only if its anchor still matches.
- If an offset is beyond file length, the cursor is cleared.
- If the anchor does not match, the cursor is cleared.
- After an unresolved command, the daemon may observe the current end as a
  future baseline, not as evidence for the unresolved command.

## Exact Command Ownership Rules

For ref-moving commands, command ownership is exact only when at least one of
these is true:

1. A pre-command cursor existed for the relevant reflog.
2. The command payload contains exact trusted command-start reflog offsets.
3. The command argv contains immutable OIDs sufficient for the specific
   operation.

Otherwise, the daemon must not attribute the command.

Examples:

- Commit after a checkpoint cursor: exact.
- Commit with no cursor and duplicate commit messages nearby: not exact.
- `merge --squash <sha>`: source is exact because argv contains immutable OID.
- `merge --squash feature` in a cold delayed command: not exact because feature
  can move.
- `stash pop stash@{0}` after later stash operations: not exact unless the stash
  stack was resolved at a cursor-bounded command boundary.

## Cold-Start Behavior

"Cold" means repo setup happened without trace2 or before the daemon knew the
repo, so the daemon has no cursor.

Correct cold behavior:

- The first traced command should be processed as a Git operation.
- The daemon should not crash, deadlock, or poison future state.
- If the first traced command lacks exact attribution evidence and no cursor
  existed before it, the daemon must fail closed for attribution.
- After that command, the daemon can observe current reflog ends and future
  commands can be exact.

Examples:

- First traced plain commit in a cold repo: no guessed authorship.
- First traced rebase in a cold repo: command can run, but rewrite attribution
  depends on exact old/new/source facts.
- First traced squash with immutable source SHA: source attribution can be
  preserved.
- First traced squash with symbolic branch source: fail closed unless a cursor
  existed.

## Checkpoint Ordering

Checkpoint processing is a real causal observation point. When a checkpoint
reaches the family actor, it can seed cursor boundaries for:

- worktree HEAD
- current branch ref
- common refs
- stash ref

Then a later ref-moving command can be matched exactly from that boundary.

This is not a hidden read-command sync. It is part of checkpoint sequencing:
the checkpoint itself is an explicit write/snapshot event in Git AI's model.

## Failed and Rejected Approaches

### mtime-guarded worktree snapshots

The original carryover snapshot race read mutable worktree files after Git
exited and guarded them with `mtime <= git_finish_time`.

Why it failed:

- filesystem clocks can be coarse
- later writes can land in the same timestamp quantum
- daemon processing is asynchronous
- the snapshot can capture content from the next operation

Rejected conclusion:

- post-commit carryover must use persisted working logs and committed tree data,
  not live worktree reads plus mtime guards

### Live worktree stash restore

Earlier stash restoration read current worktree files after `stash pop/apply`.

Why it failed:

- a later edit can occur before daemon processing
- attribution can be shifted onto newer content
- this is the same mutable-state race as post-commit carryover

Current direction:

- save stash working-log data
- reconstruct applied stash content from stash object plus target head in an
  isolated environment
- write attribution to the target working log

### Daemon-ingress synthetic reflog offsets

An attempted fix captured reflog offsets in daemon ingress when the daemon first
saw a trace2 frame.

Why it failed:

- trace2 frames reach the daemon asynchronously
- Git may already have appended the reflog entry by the time the daemon reads
  the reflog
- the captured "start" offset may actually be a post-command offset
- tests using those offsets can model a capability stock trace2 does not provide

Rejected conclusion:

- daemon ingress must not synthesize command-start reflog offsets
- tests that inject `git_ai_root_reflog_start_offsets` must be treated as
  synthetic-boundary tests, not stock trace2 behavior

### Trace2 barrier / hidden read-command sync

A trace2 barrier was explored to force production read commands to wait for
prior trace traffic.

Why it was wrong:

- production reads should not secretly sync the daemon
- tests already have explicit syncs immediately before assertions
- barriers can hide races instead of proving correctness
- a barrier does not create missing command-start data

Rejected conclusion:

- no hidden daemon sync in production read commands such as `show` or `blame`
- explicit sync remains a test/assertion tool

### Reflog timestamp matching

Reflog timestamps are seconds-resolution. They cannot distinguish same-second
commands and are not linked to trace2 root identity.

Rejected conclusion:

- timestamps can be diagnostics or secondary correlation inside an already
  exact candidate set
- timestamps must not be primary ownership proof

### Message matching without a cursor

Commit/reflog messages collide. Duplicate commit messages are common.

Rejected conclusion:

- message matching without a cursor is heuristic
- duplicate-message cold tests should fail closed rather than choosing one

## Known Good Pieces

The current design direction is good in these areas:

- family actor owns cursor state
- cursor uses offsets plus anchors
- incomplete trailing reflog lines are ignored
- truncation/pruning invalidates stale cursors
- branch delete/recreate is tested
- checkpoint can seed future cursor boundaries
- unresolved commands can seed only future baselines
- daemon-ingress offset synthesis has been removed from production direction
- hidden read-command sync/barrier has been rejected

## Current Incomplete or Risky Pieces

### Dirty/in-progress state

At the time this doc was written, the worktree had in-progress code/test changes
around cursor fail-closed behavior. Some focused suites had passed in earlier
runs, but `daemon_mode` still had known failures after those changes. The branch
should not be called proven until those are resolved.

### Synthetic offset artifacts

Some tests still inject `git_ai_root_reflog_start_offsets`. These should be
removed, quarantined, or explicitly labeled as tests for a hypothetical trusted
command-start boundary.

### Remaining timestamp use

`src/daemon/ref_cursor.rs` still parses reflog timestamps and uses them in some
direct update-ref correlation paths. Each use needs audit:

- acceptable only if old/new OIDs and cursor-bounded windows already make the
  candidate set exact
- not acceptable as primary proof

### Symbolic refs

Any delayed resolution of symbolic refs is suspect:

- branch names
- `HEAD~1`
- `stash@{0}`
- remote-tracking names

These are exact only if resolved at command time or inside a cursor-bounded
state model.

### Pull notes push when no notes exist

Recent daemon-mode failures included note-push paths treating missing
`refs/notes/ai` as an error. If there are no notes, pushing notes should be a
no-op, not a daemon failure.

## Test Requirements

Required deterministic tests:

1. delayed duplicate commit messages fail closed without cursor
2. checkpoint cursor preserves later commit attribution
3. no-cursor first traced commit does not guess ownership
4. no-cursor first traced command seeds future baseline only
5. reflog partial trailing line is ignored
6. reflog partially pruned clears invalid cursor
7. reflog fully pruned clears invalid cursor
8. branch delete/recreate clears stale cursor state
9. symbolic source ref movement after command does not corrupt attribution
10. immutable source OID remains usable for squash
11. live worktree edit after commit does not affect committed attribution
12. live worktree edit after stash pop does not affect stash restoration
13. symlink/canonical path variants map to the same repo family
14. no hidden production read sync before `show` or `blame`
15. daemon does not deadlock on partial trace2 roots or socket close ordering

## Operational Completion Requirements

Before calling trace2 ingestion complete:

1. Ref-moving command ownership uses cursor, exact immutable OIDs, or fails
   closed.
2. No daemon-ingress reflog offset synthesis remains in production code.
3. No mtime guard remains for committed/rewrite attribution.
4. No hidden production read-command sync remains.
5. Reflog parser handles incomplete lines.
6. Reflog pruning/truncation is tested.
7. Branch lifecycle cursor invalidation is tested.
8. Symbolic refs after delay are not resolved as if they were command-time data.
9. Cold-start semantics are explicit and tested.
10. Focused daemon/ref-cursor tests pass.
11. `tests/daemon_mode.rs` passes.
12. `task test` passes.

## Bottom Line

The exact trace2 ingestion answer is strict:

- use a real cursor if one existed before the command
- use immutable OIDs when the command itself contains them
- otherwise fail closed

That means Git AI cannot always attribute the first delayed write command in a
cold repo using stock trace2 alone. That is not a bug in the fail-closed model;
it is missing information. The alternative is to introduce a real trusted
command-start boundary. Anything based on latest HEAD, timestamps, messages, or
daemon-observed "start" offsets is heuristic and should not be used for
mission-critical attribution correctness.
