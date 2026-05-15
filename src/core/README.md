# Core

The attribution engine — computes, stores, and retrieves AI/human line-level authorship.

## Modules

| File | Purpose |
|------|---------|
| `attribution.rs` | Character/line-level diff and attribution algorithms (uses `imara-diff`) |
| `working_log.rs` | Checkpoint accumulator — stores per-file attribution state between checkpoints and commit |
| `authorship_log.rs` | Serialization schema (`authorship/3.0.0`) for the final git note payload |
| `post_commit.rs` | Reads working logs, resolves committed content, produces `AuthorshipLog` |
| `stash.rs` | Working log preservation across stash push/pop |

## Data flow

```
checkpoint → working_log (on disk) → post_commit → AuthorshipLog → git note
```

### Working log (`.git/ai/working_logs/<base_commit>/`)

Each checkpoint writes a JSON entry recording which lines of which files are AI, human, or untracked. Entries accumulate until the next commit.

### AuthorshipLog (git note under `refs/notes/ai`)

The final output. Contains:
- `attestation_entries`: hash → line ranges with attribution type
- `metadata`: prompt records, model info, session IDs
- Schema version for forwards compatibility

## Attribution algorithm

1. Diff the file at checkpoint time against its state at the previous checkpoint (or HEAD)
2. Map inserted/modified lines to the active checkpoint type (AI, known_human, or untracked)
3. On commit, intersect working log attributions with the actual committed diff
4. Store only lines that were committed (not speculative working-tree state)
