# Unified Rewrite Op v3 — Spec

## Problem

The current rewrite system has 5+ separate code paths (~5000 lines) handling what is fundamentally one operation: transferring attribution through a commit rewrite. It maintains a JSONL event log (rewrite_log), tracks conflict/abort/continue state, parses reflogs for segment detection, and uses heuristic commit pairing — all unnecessary complexity when git already provides plumbing commands that answer the exact questions we need.

## Core Insight

Every rewrite operation is the same transform:

```
Rewrite<T>: [Commits] → [Commits]
```

Given three points — `(onto, original_head, new_head)` — `git range-diff onto..original onto..new` tells us exactly how each commit mapped. No heuristics required. Git already solved the commit-matching problem.

## Data Model

### Input

```
RewriteTriple {
  onto:          CommitId   // common ancestor of both ranges
  original_head: CommitId   // tip of pre-rewrite range
  new_head:      CommitId   // tip of post-rewrite range
}
```

All rewrite sources produce this triple:
- `git rebase` (including interactive, autosquash, --onto)
- `git pull --rebase`
- `git cherry-pick`
- `git commit --amend`
- `git merge --squash` + commit
- `git reset` (non-ancestor, rebase-like)
- Ref updates (Graphite restack, etc.)

### Commit Mapping (from range-diff)

For each commit in the range, exactly one of:

| Symbol | Meaning | Attribution Action |
|--------|---------|-------------------|
| `=` | Identical (patch-equal) | Copy note as-is |
| `!` | Modified (content changed) | Transfer via hunks |
| `<` | Deleted (commit dropped) | No-op (attribution removed) |
| `>` | Added (new commit) | No-op (handled by post-commit flow) |

Split is `1 × <` + `N × !` (or `>`). No special case needed for split.

**Squash** is special-cased: range-diff shows `N × <` + `1 × !` (it matches the squash result against the most similar original, deletes the rest). To avoid attribution loss from the "deleted" originals whose lines survived into the squash, detect the N:1 pattern and diff the squash result against ALL originals, unioning the transferred attributions. This ensures lines from any original commit retain their attribution in the squash result.

### Hunk Transfer Invariant

For any hunk `h` carrying attribution `A`:
- **Copied** (context lines in diff): `A` preserved on new line positions
- **Deleted** (`-` lines in diff): `A` removed
- **Split** (partial overlap with diff boundary): `A` applied to surviving fragment

New lines (`+` in diff) receive no attribution from the rewrite transfer. If they were produced during conflict resolution by a checkpointed agent, attribution comes from the normal checkpoint → post-commit flow, not from rewrite transfer.

## What's Eliminated

| Component | Lines | Reason Unnecessary |
|-----------|-------|--------------------|
| `rewrite_log.rs` (JSONL event log) | 710 | No history needed — process final state only |
| `rebase_authorship.rs` (5 rewrite functions) | 4782 | Replaced by single function |
| `rebase_hooks.rs` (commit mapping heuristics) | 166 | `range-diff` handles mapping |
| Pending rebase/cherry-pick state tracking | ~200 | No conflict tracking needed |
| Segment resolution (reflog parsing) | ~300 | `range-diff` doesn't need it |
| VirtualAttributions blame fallback | ~400 | Notes + diff-tree sufficient |
| Rewrite event type dispatch | ~300 | Single code path for all ops |

## What Remains

- **Working log migration**: Still needed. Uncommitted attributions are keyed by HEAD SHA; rebase changes HEAD SHA. Rename directory from old→new.
- **`diff_based_line_attribution_transfer()`**: The core algorithm. Uses imara-diff to walk hunks and carry attributions forward on equal lines.
- **Stash attribution**: Save/restore via git notes on stash SHAs. Stash SHA at pop/apply time is available from trace2 ref-change events in the same side-effect pass — no persistence needed.
- **Post-commit flow**: Unchanged. Conflict resolution commits go through normal checkpoint → post-commit, not through rewrite transfer.
- **Merge --squash source resolution**: At commit time, reconstruct the squash source from git state (SQUASH_MSG, reflog, MERGE_HEAD) rather than holding it in daemon memory or a persistent log. Fully stateless.

## Performance Constraint

```
wall_time(v3) ≤ 0.5 × wall_time(native_git_op)
```

Achieved via:
- 1 `git range-diff` call (commit mapping)
- 1 `git cat-file` per file per modified commit (blob content)
- 1 `git notes add` per new commit (write result)

No reflog parsing. No ancestry validation. No blame subprocess. No merge-base computation.

## Conflict Resolution

Not tracked by the rewrite system. When a rebase hits conflicts:
1. Git pauses. User (or AI) resolves.
2. User runs `git rebase --continue`. Git commits the resolution.
3. Eventually rebase completes. Daemon sees `RebaseComplete` with final `(old_head, new_head)`.
4. `range-diff` maps commits including the conflict-resolution result.
5. Lines that survived from the original get their attribution transferred.
6. Lines introduced during conflict resolution have no old attribution to transfer — correct behavior.

If an AI agent resolved the conflict and fired checkpoints, those attributions flow through the normal post-commit path for that conflict-resolution commit, completely independent of the rewrite transfer.

## Decisions

| Question | Decision | Rationale |
|----------|----------|-----------|
| Squash attribution loss | Special-case N:1 | Diff against all originals, union attributions. Correctness over simplicity for this case. |
| Merge --squash source tracking | Reconstruct from git state | Stateless. Read SQUASH_MSG/reflog at commit time. No daemon memory or persistence. |
| Stash SHA at pop/apply | Trace2 events sufficient | SHA available from ref-change events in the same side-effect pass. No store needed. |
