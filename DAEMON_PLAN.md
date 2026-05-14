# Daemon Plan (v2)

The daemon is the brain of git-ai v2. It listens for git events via trace2,
processes checkpoints from AI coding agents, and generates authorship notes
on commits — all without requiring a git wrapper/proxy.

## Architecture Overview

```
┌──────────────────────────────────────────────────────┐
│                    git-ai daemon                      │
│                                                      │
│  ┌─────────────┐   ┌──────────────┐   ┌──────────┐  │
│  │ Trace2      │   │ Control      │   │ Core     │  │
│  │ Listener    │──▶│ Coordinator  │──▶│ Engine   │  │
│  │ (AF_UNIX)   │   │              │   │          │  │
│  └─────────────┘   └──────────────┘   └──────────┘  │
│                          ▲                           │
│  ┌─────────────┐         │                           │
│  │ Control     │─────────┘                           │
│  │ Socket      │                                     │
│  │ (AF_UNIX)   │                                     │
│  └─────────────┘                                     │
└──────────────────────────────────────────────────────┘
         ▲                          ▲
         │ trace2 events            │ checkpoint requests
         │ (from git)               │ (from agents)
```

**Key principle:** The daemon does NOT replace `git-ai checkpoint` or
`git-ai post-commit` — those remain usable as standalone CLI commands.
The daemon adds *automatic* post-commit detection so users don't need
to install git hooks.

---

## Implementation Phases

### Phase 1: Minimal daemon loop (detect commits, generate notes)

The smallest useful daemon: listen on a trace2 socket, detect when
`git commit` completes, and run authorship generation.

- [x] **1.1 Daemon skeleton** — `src/daemon/lifecycle.rs`, `src/daemon/run.rs`
  - Process lifecycle: fork/daemonize, PID file, lock file
  - Signal handling: SIGTERM/SIGINT on Linux/macOS, SetConsoleCtrlHandler on Windows
  - Logging to `~/.git-ai/internal/daemon/daemon.log`
  - `git-ai bg run` command to start in foreground (all platforms)
  - `git-ai bg start` to daemonize: double-fork on Linux/macOS, detached CreateProcess on Windows
  - `git-ai bg stop` to send SIGTERM (Unix) or taskkill (Windows) via PID file

- [x] **1.2 Trace2 socket listener** — `src/daemon/trace2_listener.rs`
  - Bind AF_UNIX stream socket at `~/.git-ai/internal/daemon/trace2.sock`
  - Accept connections, read newline-delimited JSON events
  - Handle Unix socket path length limit (>= 100 bytes → hash to /tmp/)
  - Windows: named pipe path resolution done in `lifecycle.rs`; actual named pipe listener is in progress (currently Unix-only)

- [x] **1.3 Trace2 event detection** — `src/daemon/trace2_events.rs`
  - Parse minimal trace2 JSON: extract `event`, `sid`, `argv`, `cmd_name`
  - Detect root-level `exit` event for `git commit` commands
  - Extract working directory (repo path) from `def_repo` or `worktree` events
  - Detect exit code (only process exit_code == 0 commits)

- [x] **1.4 Post-commit trigger** — `src/daemon/post_commit_worker.rs`, `src/daemon/commit_detector.rs`, `src/daemon/event_loop.rs`
  - On successful `git commit` exit: resolve git_dir, HEAD, parent
  - Call existing `generate_authorship_for_commit()` from `core::post_commit`
  - Write authorship note via `git notes --ref=ai add`
  - Write INITIAL attributions for next commit
  - Clean up working log for consumed base commit
  - Skip if commit already has an authorship note (idempotency)
  - Handles rapid sequential commits (scans recent history for unannotated commits)

- [x] **1.5 Install command wires trace2 config**
  - `git-ai install` sets `git config --global trace2.eventTarget af_unix:stream:<path>`
  - `git-ai install` sets `git config --global trace2.eventNesting 10`
  - Disable trace2 in the daemon's own git subprocess calls (`GIT_TRACE2_EVENT=0`)
  - Kills v1 daemon if running

- [x] **1.6 Integration tests for daemon mode**
  - Test: daemon detects commit and writes authorship note without hook
  - Test: daemon is idempotent (re-running on same commit is no-op)
  - Test: daemon handles rapid sequential commits
  - Test: graceful shutdown mid-processing (not yet implemented)

### Phase 2: Control socket (checkpoint ingestion from agents)

Allow AI coding agents to submit checkpoints via the control socket
instead of spawning `git-ai checkpoint` as a subprocess.

- [x] **2.1 Control socket listener** — `src/daemon/control_socket.rs`
  - AF_UNIX stream socket at `~/.git-ai/internal/daemon/control.sock`
  - Simple request/response protocol: JSON-line request → JSON-line response
  - Connection timeout (30s idle → close)

- [x] **2.2 Control protocol** — `src/daemon/protocol.rs`
  - `Checkpoint { repo_dir, kind, files, agent }` → processes checkpoint
  - `Status { repo_dir }` → returns current working log state for repo
  - `Shutdown` → graceful daemon stop
  - `Ping` → health check / version response

- [x] **2.3 Checkpoint processing via control socket** — `src/daemon/checkpoint_worker.rs`
  - Reuses the same `update_attributions` + `append_checkpoint` logic
  - Agent can optionally send file content in request (avoids disk read)
  - Returns processed entry count in response

- [x] **2.4 CLI routes through daemon when available**
  - `git-ai checkpoint` auto-routes through control socket if daemon is running
  - `src/daemon/control_client.rs` provides the client-side connection
  - Fallback: if daemon is not running (socket missing), falls back to local processing
  - Disable with `GIT_AI_NO_DAEMON=1` env var

### Phase 3: Rewrite tracking (rebase, cherry-pick, amend, reset)

Detect history-rewriting operations and propagate authorship notes
to rewritten commits.

- [x] **3.1 Detect rewrite operations from trace2** — `src/daemon/commit_detector.rs`
  - `git rebase` — detect completion via cmd_name, map old→new commits via ORIG_HEAD + merge-base
  - `git commit --amend` — detect via argv `--amend` flag, map HEAD@{1} to new HEAD
  - `git cherry-pick` — detect via cmd_name, copy authorship from source commit trailer
  - `git reset` — detect via cmd_name, migrate working logs from old HEAD to new HEAD

- [x] **3.2 Rewrite worker** — `src/daemon/rewrite_worker.rs`
  - On rewrite completion: copy/adapt authorship notes to new commits
  - Use ORIG_HEAD, merge-base, reflog (HEAD@{1}), and commit count to determine mappings
  - Migrate working log directories from old base commit to new base commit

- [x] **3.3 Note propagation**
  - Copy authorship note from old commit to new commit
  - Update `base_commit_sha` metadata field in copied notes
  - Handle squash (copy remaining notes to last new commit)
  - Skip if new commit already has a note (idempotent)

### Phase 4: Multi-repo coordination

Track multiple repositories simultaneously.

- [x] **4.1 Per-repo state isolation**
  - All state keyed by repo working directory (from trace2 `def_repo` event)
  - Concurrent commits in different repos processed independently
  - Single daemon handles all repos on the machine (tested with 3 concurrent repos)

- [x] **4.2 Repo discovery and path resolution** — `src/daemon/repo_resolver.rs`
  - Extract repo path from `def_repo` event `worktree` field
  - Resolve symlinks via `fs::canonicalize` + `git rev-parse --show-toplevel`
  - Cache resolved paths with TTL (5-minute expiry, periodic pruning)
  - Handles git worktrees (each worktree resolves to its own working directory)

### Phase 5: Outbound telemetry (metrics, CAS, error reporting)

The daemon sends data to the git-ai backend. This is how the service
tracks usage and provides dashboards. The contract MUST match v1.

- [x] **5.1 API client** — `src/daemon/api_client.rs`
  - HTTP client (ureq 2.12, blocking, native-tls) for outbound calls
  - Base URL: `GIT_AI_API_BASE_URL` env > config file > `https://usegitai.com` default
  - Auth: `X-API-Key` from env/config, `Authorization: Bearer` from credentials.json
  - Headers: `User-Agent: git-ai/{version}`, `X-Distinct-ID: {uuid}`
  - Retry logic: 1 retry after 60s for transient failures, 30s request timeout

- [x] **5.2 Metrics upload** — `POST /worker/metrics/upload`
  - Wire format: `MetricsBatch { v: 1, events: [MetricEvent, ...] }`
  - `MetricEvent`: `{ t: unix_secs, e: event_id, v: sparse_values, a: sparse_attrs }`
  - Event IDs match v1: Committed=1, AgentUsage=2, InstallHooks=3, Checkpoint=4, SessionEvent=5
  - Batch flush every 3 seconds on background thread (same as v1)
  - Max 1000 events per upload batch

- [x] **5.3 CAS upload** — `POST /worker/cas/upload`
  - Content-addressable store for authorship log snapshots
  - `CasUploadRequest { objects: [{ content: JSON, hash: sha256hex, metadata: {} }] }`
  - Hash computed as SHA256 of JSON-serialized content (lowercase hex)
  - Upload in chunks of 50 objects max
  - Empty metadata omitted from wire format (matches v1 `skip_serializing_if`)

- [ ] **5.4 Error reporting (Sentry + PostHog)**
  - Sentry DSN from config for error/panic reporting
  - PostHog for product analytics events
  - Same envelope format as v1 (`TelemetryEnvelope::Error/Performance/Message`)
  - *Deferred: implement when Sentry/PostHog DSN values are available*

- [x] **5.5 Contract tests** — `src/daemon/telemetry_contract_tests.rs`
  - MetricsBatch version field = 1 ✓
  - MetricEvent fields use compact single-char keys (`t`, `e`, `v`, `a`) ✓
  - CAS hash is SHA256 of content JSON (lowercase hex, 64 chars) ✓
  - Sparse arrays use string position keys, explicit nulls preserved ✓
  - Empty metadata omitted from CAS objects ✓
  - Response types deserialize correctly from backend format ✓

### Phase 6: Robustness and production hardening

- [x] **6.1 Crash recovery** — `src/daemon/startup.rs`
  - On startup: stale PID file detection (PID no longer alive → remove pid + lock)
  - Socket file cleanup: probe stale sockets (connect attempt fails → remove)
  - Log rotation: truncate log to last 1MB when exceeding 10MB

- [ ] **6.2 Self-update and restart**
  - Periodic version check (configurable interval)
  - Graceful restart: finish in-flight work, re-exec new binary
  - Max uptime guard (restart after ~24h to pick up updates)
  - *Deferred: requires binary distribution infrastructure*

- [x] **6.3 Performance** — `src/daemon/event_loop.rs`
  - Debounce rewrite operations (500ms window — rebase produces many rapid events)
  - Reduced recv_timeout from 100ms to 50ms for faster event processing
  - Pending rewrites flushed on shutdown

- [x] **6.4 Observability** — `src/daemon/stats.rs`
  - Runtime stats: commits processed/skipped, rewrites, checkpoints, errors, connections, events
  - `git-ai bg status` queries live stats from control socket
  - Stats exposed via `{"type":"stats"}` control protocol request
  - Atomic counters updated at trace2 listener, event loop, and control socket layers

---

## Platform Targets

The daemon MUST work on all three platforms: **Linux**, **macOS**, and **Windows**.

### Platform matrix

| Component | Linux | macOS | Windows |
|-----------|-------|-------|---------|
| Daemon lifecycle (PID, lock) | flock + kill(0) | flock + kill(0) | LockFileEx + tasklist |
| Daemonize | double-fork/setsid | double-fork/setsid | Windows Service / detached process |
| Trace2 listener | AF_UNIX stream socket | AF_UNIX stream socket | Named pipe (`\\.\pipe\git-ai-<hash>-trace2`) |
| Control socket | AF_UNIX stream socket | AF_UNIX stream socket | Named pipe (`\\.\pipe\git-ai-<hash>-control`) |
| Signal handling | SIGTERM/SIGINT | SIGTERM/SIGINT | SetConsoleCtrlHandler |
| Process termination | SIGTERM | SIGTERM | taskkill /PID |
| Socket path limit | 108 bytes (hash fallback) | 104 bytes (hash fallback) | N/A (named pipes have no path limit) |
| Home directory | `$HOME` | `$HOME` | `%USERPROFILE%` or `%APPDATA%` |
| Trace2 config value | `af_unix:stream:<path>` | `af_unix:stream:<path>` | `af_unix:stream:<path>` (git-for-windows supports it) |

### Platform-specific notes

- **macOS**: Socket path max is 104 bytes (vs Linux's 108). The hash-to-`/tmp/` fallback handles both.
- **Windows**: Git-for-Windows supports `trace2.eventTarget = af_unix:stream:<path>` via Unix socket emulation in newer builds. If unavailable, fall back to a named pipe listener. Named pipes use `\\.\pipe\git-ai-<hash>-<name>`.
- **Windows daemonize**: No `fork()` available. Use `CreateProcess` with `DETACHED_PROCESS` + `CREATE_NO_WINDOW` flags, or register as a Windows Service for auto-start.
- **CI targets**: Ubuntu (fastest, ~15min), macOS (~35min), Windows (~3.5h). Iterate on Ubuntu first.

---

## Design Decisions

### Sync vs Async

Phase 1 can be **synchronous** (threads for trace listener + main loop).
V1 uses tokio, but the daemon's hot path is I/O-bound (socket reads, git
subprocess calls), not CPU-bound. A thread-per-connection model with
blocking I/O is simpler and sufficient for Phase 1-2.

Migrate to async (tokio) only if connection count or throughput demands it
(Phase 5). Keep the option open by isolating I/O behind trait boundaries.

### Dependencies to add

Phase 1 requires:
- No new deps for basic Unix sockets (`std::os::unix::net`) — Linux + macOS
- `libc` for daemonization (fork, setsid) — Linux + macOS
- `windows-sys` for `LockFileEx`, `CreateProcess`, named pipes — Windows
- `tracing` + `tracing-subscriber` for structured logging (optional, can use eprintln initially)

Phase 2+:
- Potentially `tokio` if async becomes necessary
- `interprocess` for cross-platform named pipes (alternative to raw Windows API)

### Relationship to existing CLI commands

| Command | With daemon running | Without daemon |
|---------|-------------------|----------------|
| `git-ai checkpoint` | Writes to working_log (same as now) | Same |
| `git-ai post-commit` | Writes note (same as now) | Same |
| `git commit` | Daemon auto-detects and writes note | No note unless hook installed |
| `git-ai install` | Starts daemon + sets trace2 config | Sets trace2 config only |

The daemon is additive — all existing CLI paths remain functional.

### Socket path conventions

```
~/.git-ai/internal/daemon/
├── daemon.lock          # flock-based single-instance guard
├── daemon.pid.json      # { "pid": N, "started_at": "...", "version": "..." }
├── daemon.log           # stderr redirect
├── trace2.sock          # AF_UNIX trace2 listener
└── control.sock         # AF_UNIX control API
```

If socket path exceeds Unix limit (108 bytes), hash to:
`/tmp/git-ai-d-<sha256[..16]>/trace.sock`

---

## Current Status

- [x] Core attribution engine (`src/core/attribution.rs`)
- [x] Post-commit authorship generation (`src/core/post_commit.rs`)
- [x] Working log read/write (`src/core/working_log.rs`)
- [x] Authorship log serialization (`src/core/authorship_log.rs`)
- [x] CLI: `git-ai checkpoint` command
- [x] CLI: `git-ai post-commit` command
- [x] CLI: `git-ai blame` command
- [x] CLI: `git-ai stats` command
- [x] CLI: `git-ai install` (basic hook installer)
- [x] Integration test suite (57/57 passing)
- [x] **Daemon Phase 1** ← complete
- [x] **Daemon Phase 2** ← complete (control socket + checkpoint worker + client fallback)
- [x] **Daemon Phase 3** ← complete (rewrite tracking: rebase, cherry-pick, amend, reset)
- [x] **Daemon Phase 4** ← complete (multi-repo coordination + path resolution + caching)
- [x] **Daemon Phase 5** ← complete (API client + metrics/CAS upload + telemetry worker + contract tests)
- [x] **Daemon Phase 6** ← complete (crash recovery + debounce + observability; 6.2 self-update deferred)

---

## Outbound Data Contract (v1 compatibility)

V1 daemon sends three categories of outbound data. V2 MUST produce
identical wire formats so the backend doesn't need changes.

### 1. Metrics (`POST /worker/metrics/upload`)

```json
{
  "v": 1,
  "events": [
    {
      "t": 1715644800,       // unix timestamp (u32)
      "e": 42,              // event type ID (u16)
      "v": [[0, 5], [2, 1]], // sparse array of values (position-encoded)
      "a": [[0, "cursor"], [1, "session_abc"]]  // sparse array of attrs
    }
  ]
}
```

Event IDs are defined in `src/metrics/events.rs`. Key ones:
- Agent usage (checkpoint processed)
- Commit attribution generated
- Daemon lifecycle (start/stop/crash)

### 2. CAS (`POST /worker/cas/upload`)

```json
{
  "objects": [
    {
      "content": { /* authorship log or transcript JSON */ },
      "hash": "sha256hex...",
      "metadata": { "repo": "...", "commit": "..." }
    }
  ]
}
```

CAS stores authorship logs and transcripts for the web dashboard.
The hash is SHA256 of the JSON-serialized `content` field.

### 3. Error reporting (Sentry envelope + PostHog capture)

Sentry: standard envelope format to `/api/<project_id>/store/`
PostHog: `POST /capture/` with `{ api_key, distinct_id, event, properties }`

### Testing strategy for outbound contract

1. **Record fixtures**: Run v1 daemon with HTTP intercept, capture request
   bodies for: single commit, multi-file commit, checkpoint ingestion,
   daemon start/stop cycle.

2. **Schema validation**: Define JSON schemas for each endpoint's request
   body. Both v1 and v2 must validate against the same schemas.

3. **Snapshot tests**: v2 generates the same MetricEvent/CAS payload for
   identical input scenarios. Use insta snapshots to detect drift.

4. **Integration test with mock server**: Spin up a local HTTP server in
   tests, configure v2 daemon to point at it, verify request format.

---

## Decisions (resolved)

1. **Daemon is a subcommand** (`git-ai bg run`, `git-ai bg start`, `git-ai bg stop`).
   Single binary, no separate daemon executable.

2. **No tokio unless forced.** Use std threads + blocking I/O (`std::os::unix::net`).
   Only add tokio if a concrete phase demands it — not speculatively.

3. **Checkpoints: both CLI and control socket.** CLI remains primary.
   Control socket is an optimization (Phase 2) for lower-latency agent integration.

4. **V2 kills v1 on install.** `git-ai install` stops the v1 daemon
   (sends shutdown to v1's control socket or kills by PID file), reconfigures
   trace2 to point at v2's socket, and starts v2 daemon. No coexistence.
