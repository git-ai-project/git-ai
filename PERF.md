# Performance: v2 vs v1

## Environment

- Platform: Linux 6.12.86+deb13-cloud-arm64 (aarch64, Debian)
- Git version: 2.47.3
- Date: 2026-05-14
- v1 version: 1.4.7 (release build, 22MB binary)
- v2 version: 2.0.0-alpha.1 (release build, 2.3MB binary)
- CPU: ARM64 (cloud instance)
- Iterations per benchmark: 5 (median reported)

## Architecture Differences

| Aspect | v1 | v2 |
|--------|----|----|
| Checkpoint processing | Async (daemon via IPC) | Synchronous (in-process) |
| Post-commit note gen | Async (daemon, proxy polls for note) | Synchronous (explicit `post-commit` call) |
| Rebase note rewriting | Async (daemon post-rewrite hook) | Synchronous (`post-rewrite` command) |
| Blame | Direct (reads git notes) | Direct (reads git notes) |
| Binary size | 22 MB | 2.3 MB |
| Startup time | 2ms | 1ms |

**Key architectural note**: v1 offloads all heavy processing to a persistent background daemon. The CLI binary is a thin IPC dispatcher (~3ms per call). This means v1's "checkpoint latency" as seen by the caller is just IPC overhead, while actual processing happens concurrently in the background. v2 processes everything synchronously in each binary invocation.

## Results

### 1. Checkpoint Latency

v1 measures IPC dispatch to daemon (fire-and-forget); v2 measures full synchronous processing.

| File Size | v1 (IPC dispatch) | v2 (sync) | Notes |
|-----------|-------------------|-----------|-------|
| Small (10 lines) | 3ms | 7ms | v1 is IPC-only, not comparable |
| Medium (200 lines) | 3ms | 7ms | v2 overhead is mostly binary startup + git rev-parse |
| Large (2000 lines) | 3ms | 11ms | v2 shows ~4ms of actual diff/attribution work |

**v2 batch checkpoint** (all files in single invocation, avoids repeated startup):

| Scenario | Per-file (N invocations) | Batch (1 invocation) | Speedup |
|----------|--------------------------|---------------------|---------|
| 10 files, 5 lines each | 67ms | 29ms | 2.3x |
| 50 files, 10 lines each | 328ms | 130ms | 2.5x |

### 2. Full Commit Workflow (end-to-end)

Measures the total wall-clock time a user experiences: checkpoint + git add + git commit + authorship note generation.

#### Per-file checkpoint mode (typical agent usage: one checkpoint call per file edit)

| Scenario | v1 (daemon) | v2 (sync, per-file) | Delta |
|----------|-------------|---------------------|-------|
| 1 file, 5 lines | 17ms | 32ms | v2 +88% |
| 10 files, 5 lines each | 38ms | 100ms | v2 +163% |
| 50 files, 10 lines each | 124ms | 404ms | v2 +226% |

#### Batch checkpoint mode (v2 optimization: all files in single call)

| Scenario | v1 (daemon) | v2 (sync, batch) | Delta |
|----------|-------------|------------------|-------|
| 1 file, 5 lines | 17ms | 31ms | v2 +82% |
| 10 files, 5 lines each | 18ms | 63ms | v2 +250% |
| 50 files, 10 lines each | 19ms | 207ms | v2 +989% |

**Why v1 appears constant at ~18ms for batch mode**: v1's checkpoint IPC dispatch is fire-and-forget (~3ms regardless of file count). The daemon processes files concurrently in the background. By the time `git commit` runs, the daemon has already completed processing. The 18ms is essentially `IPC dispatch (3ms) + git commit (10ms) + note poll (5ms)`.

### 2b. Post-Commit Only (v2 isolated)

| Scenario | v2 post-commit | Notes |
|----------|----------------|-------|
| 1 file, 5 lines | 11ms | Reading working logs + writing git note |
| 10 files, 5 lines each | 21ms | Linear scaling with file count |
| 50 files, 10 lines each | 65ms | ~1.3ms per file |

### 3. Blame Latency

Both versions read from git notes directly -- this is a fair apples-to-apples comparison (same data format, same underlying `git blame --line-porcelain` call).

| File Size | v1 | v2 | Delta |
|-----------|----|----|-------|
| 100 lines | 22ms | 7ms | v2 -68% (3.1x faster) |
| 1000 lines | 41ms | 17ms | v2 -59% (2.4x faster) |

**v2 wins significantly on blame** because it has a much smaller binary (faster startup), fewer dependencies to initialize, and a streamlined note-parsing path.

### 4. Rebase Note Rewriting

v1 measures full `git rebase` through the proxy (git rebase execution + async daemon note rewriting, but proxy does NOT wait for note rewriting to complete). v2 measures only the synchronous `post-rewrite` note copying (after git rebase has already completed).

| Commits | v1 (rebase via proxy) | v2 (post-rewrite only) | Notes |
|---------|----------------------|------------------------|-------|
| 5 commits | 16ms | 19ms | v1 is mostly git rebase time |
| 20 commits | 46ms | 74ms | v2 note copying: ~3.7ms per commit |
| 50 commits | 106ms | 191ms | v2 note copying: ~3.8ms per commit |

**Note**: These numbers are not directly comparable. v1's timing is `git rebase` execution (the note rewriting happens async in daemon and is not measured). v2's timing is the synchronous note-copying step that happens AFTER `git rebase` completes. The real question is: what's the total latency a user sees?

Estimated total rebase time for a user:
- v1: `git rebase` time only (note rewriting is invisible, happens in daemon background)
- v2: `git rebase` time + `post-rewrite` time

For 50 commits, v2 adds ~191ms of visible latency to the rebase operation that v1 handles invisibly in the background.

## Summary

| Metric | v1 Advantage | v2 Advantage |
|--------|--------------|--------------|
| Checkpoint latency (user-perceived) | Yes (3ms IPC vs 7-11ms sync) | -- |
| Full commit workflow | Yes (18ms constant vs 31-400ms scaling) | -- |
| Blame | -- | Yes (2-3x faster) |
| Binary size | -- | Yes (10x smaller) |
| Startup time | -- | Yes (1ms vs 2ms) |
| Rebase (user-perceived) | Yes (async, invisible) | -- |
| Simplicity / no daemon needed | -- | Yes |

### Key Takeaways

1. **v1's daemon architecture provides superior latency** for write-path operations (checkpoint, post-commit, rebase) because processing is fully async and concurrent. The user never waits for attribution computation.

2. **v2 wins on read-path operations** (blame) due to its smaller binary and leaner initialization, resulting in 2-3x faster blame.

3. **v2's scaling concern**: End-to-end commit workflow time grows linearly with file count (O(n) binary invocations for per-file mode, O(n) processing time even in batch). For large changesets (50+ files), v2 adds 200-400ms of user-visible latency that v1 handles invisibly.

4. **v2's daemon mode** (not benchmarked here in isolation) would likely close the gap by processing checkpoints asynchronously, similar to v1. The numbers above represent v2's worst-case (synchronous/no-daemon) performance.

5. **Binary size reduction** (10x) benefits cold-start scenarios, CI environments, and installation UX.
