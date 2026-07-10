# Concurrent checkpoint admission

## Problem

Checkpoint side effects are already serialized by repository family. A checkpoint
waits for the trace-ingest fence, acquires `side_effect_exec_locks[family]`, enters
the family sequencer, and runs before the next entry for that family. Concurrent
checkpoints for the same worktree therefore do not run their attribution work at
the same time.

The admission path is not bounded, however. Every control connection is decoded
into a complete `CheckpointRequest` before it waits for the family execution
lock. With the default content budget, one request can retain 32 MiB, 500,000
lines, and metadata for up to 1,000 files. Enough concurrent callers can retain
many such requests while only one of them makes progress.

The active checkpoint also has bounded internal parallelism:

- up to 8 concurrent file-state hashing/blob-write jobs;
- up to 30 concurrent attribution jobs.

Those workers can consume CPU and memory, but only for the single active
checkpoint in a family. An earlier Git side effect in the same family can also
delay a checkpoint because the family sequencer preserves causal ordering.

The product control response timeout does not solve either problem. It limits
the caller's wait to two seconds, but dropping the response receiver does not
cancel the daemon task or release its captured request.

## Correctness constraints

The admission mechanism must preserve these properties:

1. Preserve the captured file contents. Re-reading the worktree when a queued
   checkpoint runs would observe later state and corrupt attribution.
2. Preserve family-sequencer order relative to trace2 commands and other
   checkpoints.
3. Never drop or coalesce an AI post-edit checkpoint. It is the evidence that
   assigns an edit to an agent.
4. Do not assume two pre-edit snapshots are equivalent merely because their
   paths, tool, or worktree match. Concurrent tools may have observed different
   contents.
5. Keep admission work off the trace2 ingestion path. This is a checkpoint
   control-path concern.
6. Bound both memory and disk use. An unbounded disk queue only moves the
   failure mode.

## Proposed design

Spool admitted checkpoint payloads and queue lightweight descriptors.

1. After control-request decoding and family resolution, serialize the captured
   request to a daemon-owned spool file using a blocking worker. Use atomic
   create/write/rename so the sequencer never observes a partial payload.
2. Replace the full request held across the trace-ingest fence and family lock
   with a descriptor containing the spool path, byte count, family/worktree key,
   trace ID, and arrival ID.
3. Insert that descriptor into the existing family sequencer. Load exactly one
   payload immediately before its side effect runs, then delete it on success or
   terminal failure.
4. Track per-worktree and global queued bytes and request counts. Reserve quota
   before writing; release it when the spool file is deleted.
5. When quota is exhausted, return an explicit busy response and emit metrics.
   This is preferable to an out-of-memory daemon, but the quota should be sized
   to make rejection exceptional because current agent hooks do not guarantee a
   retry.
6. On daemon startup, remove orphaned spool files from the previous process.
   Checkpoint control requests are currently best-effort across daemon crashes,
   so replaying an orphan without its original sequencer position would be less
   correct than reporting the loss.

This first version should not coalesce requests. A later optimization may remove
provably byte-identical pre-edit snapshots only if equality includes repository
family, worktree, base commit, path role, ordered paths, captured contents, and
the sequencer interval. The memory win is unlikely to justify that semantic
complexity before spool metrics show a need.

## Delivery plan

1. Add gauges and histograms for admitted requests, queued bytes, queue wait,
   processing duration, timeout count, and quota rejection count.
2. Add a `CheckpointSpool` abstraction with atomic persistence, quota
   reservations, cleanup, and focused unit tests.
3. Change only the checkpoint control path and `FamilySequencerEntry::Checkpoint`
   to carry descriptors. Do not change trace2 ingestion.
4. Add `TestRepo` coverage that blocks a family behind a pending root, submits
   concurrent large pre/post checkpoint pairs, verifies bounded resident queue
   state, releases the root, and asserts exact line attribution after every
   commit.
5. Stress the implementation with multiple worktrees in one family and separate
   repository families to verify ordering and independent progress.

## Immediate mitigation

The Bash-only processing deadline limits one active Bash checkpoint to a
10-second cooperative budget. Deadline checks surround working-log reads, file
state capture, hashing, attribution, and append. It prevents a pathological
active Bash checkpoint from occupying the family indefinitely, but it does not
bound the memory held by requests that have not yet acquired the family lock.
