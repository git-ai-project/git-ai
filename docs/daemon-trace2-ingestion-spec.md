# Daemon Trace2 Ingestion Spec

Status: authoritative spec for how the git-ai daemon establishes exact
ownership of git ref transitions from trace2 event streams. Companion docs:

- `docs/rewrite-ops-spec.md` — what happens once exact transitions are known.
- `docs/attribution-fuzzer-spec.md` — correctness pressure.

## The problem: exact command ownership

The daemon learns about git commands asynchronously: git writes trace2 events
to a socket, the daemon reads them later. Git mutates refs synchronously
inside the git process. By the time the daemon processes a command, the repo
may have moved on — more commands, user edits, other worktrees.

Stock trace2 does not include created commit SHAs or complete ref-update OIDs
for most commands. A delayed trace2 root saying "git commit ran in repo X
with argv Y, exit 0" identifies *that a command ran*, not *which reflog
entries it produced*. Attribution requires the latter.

**Ownership rule.** A ref-moving command's transitions are exact if and only
if at least one of:

1. **Pre-command cursor** — the daemon held a reflog cursor (byte offset +
   anchor) for the relevant ref from *before* the command; entries appended
   after the cursor, matching the command's expected transition shape, belong
   to it.
2. **Immutable argv OIDs** — the command line itself contains full OIDs
   sufficient to identify the operation (e.g. `merge --squash <sha>`,
   `update-ref ref <new> <old>`, `cherry-pick <sha1> <sha2>`).

Otherwise the command is **not exact**: the daemon must fail closed for
attribution (no guessed authorship, no note migration) and may only use the
command as a *future baseline* — observe the current reflog ends so the *next*
command is exact.

Banned as ownership proof (each was tried and failed; see Postmortem):
reflog timestamps (seconds-resolution, not causally tied to a trace2 root),
commit/reflog message matching (messages collide), latest-HEAD guessing, and
daemon-ingress "start" offsets captured after the fact.

## Data path

```
git (trace2 socket target)
  → bounded socket listener (src/daemon.rs)
      prepare_trace_payload_for_ingest: filters definitely-read-only roots,
      enqueues mutating roots with sequence numbers
  → TraceNormalizer (src/daemon/trace_normalizer.rs)
      groups frames by root sid; terminal event → NormalizedCommand
  → coordinator → family actor (one actor per repo family = common git dir)
      owns ordered state for the family, including the RefCursor
  → RefCursor::enrich_command (src/daemon/ref_cursor.rs)
      consumes cursor-bounded reflog entries → cmd.ref_changes
  → analyzers (src/daemon/analyzers/history.rs)
      classified semantic events → rewrite/post-commit side effects
```

The listener rejects trace frames larger than 64 KiB, retains at most 64 root
SIDs per connection, and admits at most 128 live trace connections. Unix and
Windows connection handlers use bounded stacks. Exceeding any limit closes the
affected connection and fails closed for attribution; it never delays or wraps
the Git command. Control connections are independently capped at 16, with a
36 MiB request limit that accommodates the default 32 MiB checkpoint budget.

Git subprocesses used later by asynchronous side effects are also bounded and
never run on trace ingress. At most eight buffered internal Git commands run at
once. Each accepts at most 16,384 arguments / 4 MiB of argument data and 32 MiB
of stdin, retains at most 32 MiB of stdout and 1 MiB of stderr, and uses bounded
reader-thread stacks. Overflow is drained, reported as an error, and fails the
affected attribution side effect closed without retaining the excess output.

Rewrite side effects additionally cap parsed work at 4,096 commits, mappings,
or diff pairs. Revision walks request at most one extra record so overflow is
detected before note maps and diff structures are built. Ordinary
cherry-pick/revert source expressions share one `cat-file` batch; at most one
range walk is allowed alongside that batch because combining independent
ranges changes Git's exclusion semantics. Multiple range arguments fail the
attribution side effect closed instead of spawning Git once per range.

Batched repository reads accept at most 4,096 paths or objects. A batch may
materialize at most 16 MiB of path-keyed content after Git object deduplication;
this prevents one shared large blob from being cloned once per path into an
otherwise unbounded map. Index parsing is rejected above 32 MiB, and staged
content uses the same constant two-process batch path rather than spawning Git
once per file.

Authorship-note reads and writes use the same 4,096-item and 16 MiB
materialization limits. The byte budget is applied after note-blob
deduplication, across cache, HTTP, and Git fallback sources, so one shared note
blob cannot be cloned once per commit. The notes SQLite cache enforces the
budget while rows are read and rolls back queue locks if a pending batch is too
large. HTTP note requests are limited to 100 entries before JSON serialization.

Separation of concerns:

- The **normalizer** parses trace2/argv facts only. It never reads mutable
  repo state to synthesize missing command facts.
- The **family actor** owns all ordered, stateful reasoning: the ref cursor,
  the stash stack, pending operation state. Commands and checkpoints for one
  repo family are processed in arrival order.
- **Side effects** run only after enrichment, on exact data.

### NormalizedCommand

Facts: family/scope, worktree, root sid, raw argv, primary command, observed
child commands, exit code, trace start/finish timestamps, optional
`reflog_start_offsets`, operation-specific immutable OIDs from argv
(stash target, cherry-pick sources, revert sources), `ref_changes` (output of
enrichment), confidence.

`reflog_start_offsets` carries *claimed* command-start reflog positions.
Trust is decided at the cursor, not at ingress (see below).

## Ref cursor model

Per family, the cursor stores per-ref-key:

- byte offset into the reflog file (always at a line boundary)
- anchor: the old/new OIDs, end offset, and a SHA-256 message fingerprint for
  the reflog record ending at that offset, proving the offset still belongs to
  the same reflog generation without retaining an unbounded message
- consumed offsets/anchors (entries already owned by earlier commands)
- in-memory stash stack and pending cherry-pick source OIDs

Ref keys distinguish `worktree:<git_dir>:HEAD` (per-worktree HEAD reflogs)
from `common:<ref>` (shared refs like `refs/heads/main`, `refs/stash`).

Robustness requirements (all implemented and tested):

- incomplete trailing reflog lines are ignored (a writer may be mid-append)
- a saved offset is honored only if it lands on a newline and its anchor
  matches the record ending there; otherwise the cursor is cleared
- offset beyond file length (pruned/truncated reflog) clears the cursor
- branch delete/recreate clears the stale cursor
- expiry/`reflog expire` invalidates via anchor mismatch
- one cursor read covers at most 4 MiB and one reflog record at most 64 KiB;
  exceeding either limit fails closed, seeds the cursor at the current EOF,
  and lets the next command recover without rescanning oversized history
- common-ref discovery is iterative and bounded to 8,192 filesystem entries,
  4,096 reflogs, 4,096 pending directories, and 64 directory levels

### Consumption

`enrich_command` consumes reflog entries appended after the cursor that match
the command's *expected transition shape* — per-command message-prefix
families (`commit`, `commit (amend):`, `rebase`, `reset:`, `checkout:`, ...)
and expected old/new OID constraints derived from family state and argv.
Matched entries move into the consumed set so the next command cannot claim
them. Multi-entry operations (rebase, multi-pick) consume contiguous spans
where each entry's `old` equals the previous entry's `new`. Message prefixes
and timestamps may *narrow* an already-exact candidate set; they are never
the proof of ownership by themselves.

If nothing matches: `ref_changes` stays empty, confidence stays low, side
effects skip attribution (fail closed), and the cursor advances to the
current reflog end as a baseline for future commands only.

### Seeding

Cursors come into existence at trusted observation points:

1. **Trace ingress capture** (best-effort): when the daemon sees a *live*,
   non-terminal trace2 frame for a mutating command, it captures current
   reflog ends and attaches them to the root as claimed start offsets. This
   latency-sensitive capture never inventories the ref tree: it performs
   constant work for the worktree `HEAD`, its current symbolic ref, and one
   auxiliary ref: `refs/stash` normally, or the explicit local source ref for
   `merge --squash`. The symbolic `HEAD` read is capped at 4 KiB. Because
   delivery is asynchronous, these claims may already be post-append;
   `command_start_offset_is_authoritative` only accepts a claimed offset if
   records exist after it and it does not move an existing cursor backward.
   An accepted-but-late offset can only shrink the window a command may claim
   (it can lose attribution, never steal another command's entries) — late
   capture is *conservative*, satisfying fail-closed.
2. **Checkpoints**: a checkpoint arriving at the family actor is a real,
   ordered causal observation; processing it establishes family state (and
   hence expected-transition inputs) for subsequent commands.
3. **Command completion**: after any command is processed — resolved or not —
   the cursor observes the current reflog ends as the baseline for the next
   command.

### Cold start

"Cold" = the repo was set up without trace2 (or before the daemon existed),
so no cursor predates the first traced command.

- The first traced command must process without crash, deadlock, or state
  poisoning.
- If it lacks immutable argv OIDs and no cursor existed, it fails closed for
  attribution and seeds the baseline.
- Subsequent commands are exact.
- Special case: first traced command whose argv contains sufficient immutable
  OIDs (e.g. `merge --squash <sha>`) is exact even cold.

## Operation-specific ownership notes

- **commit / amend**: expected HEAD transition with `commit`/`commit (amend):`
  message family; branch ref entry consumed alongside HEAD when they describe
  the same transition.
- **rebase**: consumes the contiguous HEAD span from original tip through
  final tip; `rebase --continue` of an in-progress rebase relies on
  family-actor pending state from the failed command's consumed prefix, never
  on reading `.git/rebase-merge` after the fact.
- **cherry-pick / revert**: source OIDs from argv when immutable; symbolic
  sources resolved only at a cursor-bounded boundary; pending source state
  carries across conflicted stop/continue.
- **stash**: the in-memory stash stack mirrors `refs/stash` mutations observed
  through consumed entries; `stash@{N}` resolves against that stack, not
  against the live ref at processing time.
- **reset**: `reset:` message family, old-OID constraint relaxed (reset can
  move from any state); backward-reset detection happens downstream from the
  exact transition.
- **update-ref**: argv carries ref name and usually both OIDs (immutable);
  used by graphite-style restacks (`commit-tree` + `update-ref`). Multiple
  same-command ref updates are correlated by OID first, with the command's
  time window only narrowing candidates.
- **pull**: decomposes into fetch + merge/rebase; ownership follows the
  underlying HEAD/branch transitions.
- **merge-tree / commit-tree alone**: create objects, move no refs — no
  transition to own; nothing happens until a ref moves.
- **push / fetch / clone**: notes-sync side effects keyed off argv remotes;
  missing `refs/notes/ai` is a no-op, not an error.

## Reads must not sync

Production read commands (`show`, `blame`, `status`, ...) must not trigger a
hidden daemon sync or barrier. Tests use explicit sync immediately before
assertions; a barrier in production would hide races instead of fixing them
and cannot create missing command-start data anyway.

## Postmortem: rejected approaches

These were implemented, found unsound, and removed. Do not reintroduce.

1. **mtime-guarded worktree snapshots** (post-commit carryover): read the live
   worktree after git exited, guarded by `mtime <= git_finish_time`.
   Filesystem clocks are coarse; later writes land in the same quantum; the
   snapshot can capture the *next* operation's content. Replaced by persisted
   working logs + committed tree data.
2. **Live-worktree stash restore**: same race, same fix — reconstruct from
   stash objects + target head in isolation.
3. **Daemon-ingress offsets as proof**: a captured "start" offset may actually
   be post-command. Demoted from proof to conservative, validated hint (see
   Seeding); never primary evidence.
4. **Trace2 barrier / hidden read sync**: hides races; doesn't create data.
5. **Reflog timestamp matching as proof**: seconds-resolution, collides;
   allowed only to narrow already-exact candidate sets.
6. **Message matching without a cursor**: duplicate commit messages are
   ubiquitous; cold duplicate-message commands fail closed instead.

## Test obligations

Deterministic tests must cover, at minimum:

1. delayed duplicate-message commits without a cursor fail closed
2. checkpoint-then-commit attributes exactly
3. cold first traced commit does not guess; seeds baseline only
4. partial trailing reflog line ignored; partial and full prune clear cursor
5. branch delete/recreate clears cursor state
6. symbolic ref movement after a delayed command does not corrupt attribution
7. immutable argv OIDs work cold (squash, cherry-pick, update-ref)
8. live worktree edits after commit/stash-pop do not leak into attribution
9. symlink/canonical path variants map to one repo family
10. no hidden sync before `show`/`blame`
11. daemon survives partial trace roots, socket close ordering, child trace
    traffic, and never deadlocks checkpoints behind unidentified sockets
12. oversized historical reflogs remain memory-bounded, fail closed for the
    affected boundary, and preserve exact attribution for the next command
13. repeated large file blobs remain bounded during batched materialization
14. repeated large note blobs remain bounded during rewrite-note migration,
    fail closed, and preserve attribution for the next command

Primary suites: `tests/daemon_mode.rs`, `tests/commit_tree_update_ref.rs`,
`tests/integration/rewrite_ops_attribution.rs`, ref-cursor unit tests in
`src/daemon/ref_cursor.rs`.

## Bottom line

Exactness is structural, not heuristic: cursor, or immutable argv OIDs, or
fail closed. The unavoidable consequence — git-ai cannot attribute the very
first delayed write command in a cold repo from stock trace2 alone — is
missing information, not a bug. Every mechanism that tried to paper over that
gap (timestamps, messages, latest-state guesses, post-hoc offsets) produced
misattribution and was removed.
