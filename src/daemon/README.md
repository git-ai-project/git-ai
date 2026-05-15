# Daemon

Background service that listens for git events via trace2 and processes them asynchronously.

## Architecture

The daemon is a single-process event loop that receives git trace2 events over a Unix socket (or named pipe on Windows), then dispatches work to internal workers.

```
git (trace2) → Unix socket → trace2_listener → event_loop → workers
```

## Key components

| File | Purpose |
|------|---------|
| `run.rs` | Main daemon entry point |
| `startup.rs` | Process spawning, daemonization, PID file |
| `lifecycle.rs` | Start/stop/restart orchestration |
| `event_loop.rs` | Central dispatch: receives events, routes to workers |
| `trace2_listener.rs` | Unix socket listener parsing git trace2 JSON events |
| `trace2_listener_win.rs` | Windows named pipe equivalent |
| `trace2_events.rs` | Trace2 event type definitions |
| `protocol.rs` | Wire protocol between CLI and daemon |
| `control_socket.rs` | Control channel for `git-ai bg status/stop` |
| `control_client.rs` | Client side of control channel |

## Workers

| File | Purpose |
|------|---------|
| `checkpoint_worker.rs` | Processes checkpoint events (file attribution snapshots) |
| `post_commit_worker.rs` | Generates authorship notes after commit |
| `rewrite_worker.rs` | Rewrites notes after rebase/cherry-pick/amend |
| `stash_worker.rs` | Preserves working logs across stash operations |
| `telemetry_worker.rs` | Sends anonymous usage telemetry |
| `commit_detector.rs` | Detects new commits from trace2 events |
| `repo_resolver.rs` | Maps trace2 session IDs to repository paths |

## Why a daemon?

Without a daemon, every git operation would need a hook script that spawns a process, reads state, and writes notes. The daemon:
- Eliminates per-command startup cost (binary is already loaded)
- Receives events passively via trace2 (no hook scripts to install for git operations)
- Can batch and deduplicate work (e.g., rebase producing many commits)
- Holds working state in memory between checkpoint and commit

## Coordination with hooks

The daemon writes `.git/ai/noted/<sha>` marker files after generating a note. The synchronous post-commit hook checks for this marker and skips if the daemon already handled the commit. This prevents duplicate notes when both paths fire.
