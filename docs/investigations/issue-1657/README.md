# Issue #1657 — Windows console-window popups: investigation & fix attempt

Visible console/PowerShell windows appear repeatedly on Windows when Codex/Claude
hooks invoke `git-ai.exe checkpoint …` (reported on git-ai 1.6.1).

## Contents

| File | What it is |
|------|------------|
| `regression-analysis.html` | Standalone visual report: timeline, what-changed table, causation diagram, root cause, and the fix. Open in any browser. |
| `Dockerfile` | Builds `git-ai` from the repo and runs the lock-failure reproduction. |
| `repro-daemon-lock.sh` | The reproduction scenario (a root-owned daemon lock inside an unprivileged user's home). |

## TL;DR

**Is it a regression?** *Partly.* The fundamental "a console-subsystem `git-ai.exe`
launched per hook by a console-less agent flashes a window" issue is **pre-existing**
(unchanged since ≤1.5.8). But **1.6.0 reworked the Windows daemon launch** (PR #1587,
"Spawn Windows daemon without PowerShell") and is the most likely **amplifier**:

1. It pairs `CREATE_NO_WINDOW` with `DETACHED_PROCESS`. Per Microsoft's `CreateProcess`
   docs, `CREATE_NO_WINDOW` is *ignored* when combined with `DETACHED_PROCESS` — so
   window suppression now rests entirely on `DETACHED_PROCESS`.
2. It sits in front of a daemon-startup path that **silently mis-reports permission
   failures as "lock held"**, so on machines where `~/.git-ai/internal` is owned by an
   elevated/other user the daemon never starts and *every* hook re-attempts the visible
   launch. This matches the reporter's diagnostics (`lock held` **and** `Access is
   denied (os error 5)`) and issue #1287.

## The fix in this branch

Targets the amplifier — the highest-confidence, testable defect:

- `src/utils.rs`: new `LockAttempt { Acquired, Contended, Inaccessible }` and
  `LockFile::try_acquire_detailed()`. The lock helpers now distinguish a genuinely
  **held** lock (Windows `ERROR_SHARING_VIOLATION`/`ERROR_LOCK_VIOLATION`, Unix
  `EWOULDBLOCK`) from an **inaccessible** one (permission/IO error).
- `src/commands/daemon.rs`: `daemon_lock_state()` classifies the lock; both daemon
  startup paths now emit an accurate, actionable error on the permission case instead
  of the misleading "lock held".

Deliberately **not** included here (documented as follow-ups in the HTML report):
a GUI-subsystem hook launcher to suppress the per-hook console flash itself, and a
re-evaluation of the `DETACHED_PROCESS`/`CREATE_NO_WINDOW` combo.

## Running the reproduction

```bash
# from the repo root
docker build -f docs/investigations/issue-1657/Dockerfile -t gitai-1657 .
docker run --rm gitai-1657
```

- On **this branch** the script prints `✅ FIXED BEHAVIOR` (permission problem reported).
- Build it against **v1.6.1** to see `❌ BUGGY BEHAVIOR` (the phantom "lock held").

  ```bash
  git worktree add /tmp/gitai-161 v1.6.1
  docker build -f docs/investigations/issue-1657/Dockerfile -t gitai-161 /tmp/gitai-161
  docker run --rm gitai-161
  ```

The Linux scenario is the cross-user analogue of the Windows Administrator-install case:
the lock file is owned by `root` inside an unprivileged user's home, **no daemon is
running**, so any "lock held" report is provably wrong.
```
