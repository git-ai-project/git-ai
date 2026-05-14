# v2 Rewrite

## Why rewrite

v1 grew to 104K lines of Rust across 35 direct dependencies, with a 92 MB binary that acts as a git proxy (symlinked as `git` system-wide). The proxy architecture intercepts every git invocation — even ones that have nothing to do with AI authorship — and introduces a shim layer between the user and git. This creates fragility: every git upgrade, every edge case in git's CLI surface, and every signal-handling subtlety becomes our problem.

v2 eliminates the proxy entirely. It uses git's native trace2 event system to observe commits passively, and only intervenes when authorship data needs to be recorded.

## Architecture changes

### No more git proxy

v1 installs itself as `git` on the user's PATH and forwards every command to the real git binary, injecting pre/post hooks. This means:
- Every `git status`, `git log`, `git fetch` pays the overhead of loading a 92 MB binary
- Signal forwarding, process group management, and exit code propagation must be perfect
- Any bug in the proxy layer breaks the user's entire git workflow

v2 uses git's built-in `trace2` target system. A lightweight daemon listens on a Unix socket for trace2 events. Only commit-related events trigger processing. All other git operations are completely untouched — git runs natively with zero interception.

### Minimal dependencies

| | v1 | v2 |
|-|----|----|
| Direct dependencies | 35 | 5 |
| Total dependency tree | 721 crates | 65 crates |
| Source lines | 104K | 18K |
| Binary size | 92 MB | 2.4 MB |
| Clean build | 63s | 8s |

v2 depends on: `serde`, `serde_json`, `sha2`, `imara-diff`, `libc`, `glob`. No HTTP client, no SQLite, no git library, no async runtime.

### Filesystem-first design

v1 spawns git subprocesses for basic discovery (repo root, HEAD SHA, git dir). Each spawn costs ~3ms.

v2 reads the filesystem directly:
- Walks up from CWD to find `.git` (handles both normal repos and worktrees)
- Reads `HEAD` file, resolves symbolic refs through loose refs and packed-refs
- Handles worktree `commondir` resolution without any subprocess

Result: the checkpoint hot path has zero git process spawns.

### Daemon coordination via marker files

Both the daemon (trace2 listener) and the post-commit hook can process a commit. They coordinate via marker files at `.git/ai/noted/<sha>` — whichever runs first writes the marker, the other skips with a single `stat()` call.

## Performance wins

| Operation | v1 | v2 | Winner |
|-----------|----|----|--------|
| Checkpoint (hot path) | 2ms | 3ms | Tie |
| Post-commit (daemon) | 2ms | 1ms | v2 |
| Post-commit (sync) | 2ms | 3ms | Tie |
| Blame (100 lines) | 22ms | 6ms | v2 (3.7x) |
| Blame (1000 lines) | 64ms | 16ms | v2 (4x) |

See [BENCHMARKS.md](BENCHMARKS.md) for full methodology and numbers.

## Reliability wins

- No proxy means git never breaks. If the daemon crashes, git continues working normally — authorship just isn't recorded until the daemon restarts.
- Sync fallback: if the daemon isn't running, the post-commit hook handles authorship directly. Users never lose data.
- No signal forwarding bugs. v1 had to handle SIGTERM/SIGINT/SIGHUP/SIGQUIT propagation to child processes across Unix and Windows. v2 doesn't sit between the user and git.
- No argv[0] dispatch. v1's entire behavior depends on whether it was invoked as `git` or `git-ai`. v2 is always `git-ai`.

## Maintainability wins

- 82% less code (18K vs 104K lines)
- 8x faster builds (8s vs 63s clean)
- 91% fewer dependencies (65 vs 721 crates in the tree)
- No cross-platform proxy complexity. v1 has 63 `#[cfg(windows)]` annotations across 17 files for process creation, signal handling, and path normalization in the proxy layer.
- Tests run in 5 seconds (553 integration tests). v1's test suite takes significantly longer due to binary size and compilation time.

## Test parity

All 553 e2e integration tests from v1 pass on v2 with identical behavior:
- Checkpoint attribution (AI, known human, untracked)
- Post-commit note generation
- Rebase/cherry-pick/reset/stash authorship rewriting
- Worktree support
- UTF-8 filenames
- Daemon idempotency
- Cloud agent background attribution

## Migration path

v2 is a drop-in replacement. Same note format (`refs/notes/ai`, schema `authorship/3.0.0`), same checkpoint CLI interface, same agent presets. Existing authorship notes created by v1 are read correctly by v2's blame command.

Install: `git-ai install` registers the trace2 target and starts the daemon. Uninstall: `git-ai uninstall` removes the trace2 config and stops the daemon. No symlinks, no PATH manipulation.
