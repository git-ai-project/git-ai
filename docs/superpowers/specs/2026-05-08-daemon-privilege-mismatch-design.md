# Daemon Privilege Mismatch Prevention

## Problem

When the daemon is started as a privileged user (root/administrator), non-privileged users enter an unrecoverable broken state:

1. Can't connect to daemon (socket permission denied)
2. Can't start new daemon (lock file held by privileged process)
3. Can't kill old daemon (insufficient privileges to signal elevated process)

This occurs on both macOS/Linux (sudo, root shells) and Windows (Run as Administrator, elevated terminals).

## Solution: Four-Layer Defense

### Layer 1: Privilege De-escalation at Startup

The primary defense. Ensures the daemon always runs as the real user regardless of how it was invoked.

#### Unix (macOS/Linux)

On daemon startup (`run_daemon()` entry), before acquiring the lock:

1. Check `geteuid() == 0`
2. If `SUDO_UID` and `SUDO_GID` env vars exist:
   - Call `setgroups(&[gid])` to reset supplementary groups
   - Call `setgid(sudo_gid)` to drop group privileges
   - Call `setuid(sudo_uid)` to drop user privileges
   - Order matters: `setgroups` before `setgid` before `setuid` (can't change groups after dropping root)
   - Log warning: "Dropping root privileges to user {uid}"
3. If `geteuid() == 0` but no `SUDO_UID`:
   - True root login (no real user to drop to)
   - Refuse to start unless `daemon_allow_root` feature flag is enabled
   - Error: "Refusing to start daemon as root without a real user to de-escalate to. Use --allow-root to override."

#### Windows

On daemon startup, before acquiring the lock:

1. Call `OpenProcessToken(GetCurrentProcess())` + `GetTokenInformation(TokenElevation)` to detect UAC elevation
2. If elevated:
   - Call `GetTokenInformation(TokenLinkedToken)` to obtain the non-elevated linked token
   - Re-spawn the daemon binary (`git-ai bg run --respawned`) using `CreateProcessWithTokenW` with the linked token
   - Parent process waits up to 3 seconds for child to signal readiness (child acquires lock = ready)
   - Parent exits with success
3. If elevated but no linked token (true admin account, not UAC-elevated):
   - Refuse to start unless `daemon_allow_root` feature flag is enabled
   - Error: "Refusing to start daemon with administrator privileges. Use --allow-root to override."

The `--respawned` internal flag prevents infinite re-spawn loops: if the child is still elevated after respawn (shouldn't happen, but defensively), it proceeds without another respawn attempt.

### Layer 2: Socket/Lock Permission Relaxation

Belt-and-suspenders for edge cases where de-escalation isn't triggered (e.g., `su` without `SUDO_UID`, partial failures).

#### Unix

After creating socket files and lock file:
- `chmod 0660` on socket files (owner + group read/write)
- `chmod 0660` on lock file
- Since `~/.git-ai/` is already user-scoped (directory perms 0700 by default), this doesn't expand access beyond the user's home

#### Windows

Named pipes inherit the creating user's security descriptor by default. When creating from an elevated context (if de-escalation somehow didn't fire):
- Explicitly set the pipe's DACL to include `GENERIC_READ | GENERIC_WRITE` for the user's non-elevated SID
- This allows the same user's non-elevated processes to connect

### Layer 3: Privilege Mismatch Detection (Actionable Error)

When `daemon_startup_is_blocked()` returns true AND `daemon_is_up()` returns false — the "stuck" state:

1. Read `daemon.pid.json` to get the daemon PID
2. Determine if the process is running under a different/elevated privilege level:
   - **Unix**: `kill(pid, 0)` → if `EPERM`, process exists but caller lacks permission. Additionally check `/proc/{pid}/status` (Linux) or use `sysctl` KERN_PROC (macOS) to read process UID.
   - **Windows**: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, pid)` → if access denied, attempt to read token elevation status via snapshot
3. If privilege mismatch confirmed, show platform-specific remediation:
   - **Unix**: `"Daemon (PID {pid}) is running as a different user (uid {owner_uid}). To fix: sudo kill {pid} && rm {lock_path}"`
   - **Windows**: `"Daemon (PID {pid}) is running with elevated privileges. Open an Administrator terminal and run: git-ai bg stop"`

### Layer 4: Dead Process Auto-Recovery

Handles post-reboot or crash scenarios where lock file persists but process is gone:

1. Read PID from `daemon.pid.json`
2. Check if PID is actually alive:
   - **Unix**: `kill(pid, 0)` returns `ESRCH` (no such process) → dead
   - **Windows**: `OpenProcess` returns null with `ERROR_INVALID_PARAMETER` → dead
3. If confirmed dead:
   - Remove stale lock file
   - Remove stale socket files
   - Log: "Cleaned up stale daemon files from dead process {pid}"
   - Retry normal startup
4. If alive but inaccessible (`EPERM` / `ERROR_ACCESS_DENIED`):
   - Fall through to Layer 3 (privilege mismatch error)

## Execution Flow

On every `ensure_daemon_running()` call:

```
1. Am I elevated?
   ├─ Yes + can de-escalate → de-escalate, continue as real user
   ├─ Yes + true root/admin + daemon_allow_root=false → refuse with error
   └─ No → continue
2. Try to connect to running daemon
   ├─ Connected → done (normal path)
   └─ Failed → continue
3. Try to acquire lock
   ├─ Acquired → start daemon normally
   └─ Blocked → continue
4. Read PID, check if daemon process is alive
   ├─ Dead → cleanup stale files, retry from step 3
   └─ Alive but inaccessible → continue
5. Privilege mismatch?
   ├─ Yes → show actionable platform-specific kill/stop command
   └─ No → generic "daemon already running" error
```

## Configuration

### Feature Flag: `daemon_allow_root`

Controlled via the standard feature flag system (`define_feature_flags!` macro):

- **Config file**: `~/.git-ai/config.json` → `"feature_flags": { "daemon_allow_root": true }`
- **Environment variable**: `GIT_AI_DAEMON_ALLOW_ROOT=true`
- **Default**: `false` (both debug and release)

This is preferred over a CLI flag because:
1. The daemon restarts itself (after updates, on `Restart` exit action, via `ensure_daemon_running()`)
2. All restart paths would need to thread a `--allow-root` flag through spawn logic
3. A feature flag is read from `Config::get()` at the privilege check site — works everywhere automatically

Intended for CI/container environments where root is expected.

### Internal Flag: `--respawned`

`git-ai bg run --respawned` — Internal flag (Windows only) to prevent infinite re-spawn loops after de-escalation. Not user-facing.

## Platform-Specific Implementation Notes

### Unix De-escalation

```
setgroups([sudo_gid])  // must happen before setuid
setgid(sudo_gid)       // drop group first
setuid(sudo_uid)        // drop user last (irreversible)
```

After `setuid()`, the process cannot re-escalate. This is the standard privilege-drop pattern used by sshd, nginx, etc.

### Windows Token Handling

The linked token approach:
1. `OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token)`
2. `GetTokenInformation(token, TokenLinkedToken, ...)` → gives non-elevated token handle
3. `CreateProcessWithTokenW(linked_token, ..., "git-ai bg run --respawned", ...)`

Key: `TokenLinkedToken` is only available when the process was UAC-elevated (split token). For full admin accounts without split tokens, there is no linked token — this is the "refuse unless --allow-root" case.

### PID Liveness Detection

| Platform | Dead | Alive (accessible) | Alive (inaccessible) |
|----------|------|--------------------|-----------------------|
| Unix | `kill(0)` → `ESRCH` | `kill(0)` → 0 | `kill(0)` → `EPERM` |
| Windows | `OpenProcess` → null + `ERROR_INVALID_PARAMETER` | Success | null + `ERROR_ACCESS_DENIED` |

### macOS Process UID Lookup

macOS doesn't have `/proc`. Use:
```
sysctl CTL_KERN, KERN_PROC, KERN_PROC_PID, pid
```
Returns `kinfo_proc` with `kp_eproc.e_ucred.cr_uid`.

## Testing Strategy

- Integration tests: Use `GIT_AI_DAEMON_ALLOW_ROOT=true` env var or `GIT_AI_TEST_CONFIG_PATCH` with feature_flags override to bypass refusal in test environments that run as root
- Unit tests for privilege detection functions (mock `geteuid()` return values via feature flag or env var)
- Manual testing matrix:
  - macOS: `sudo git-ai bg start` → verify de-escalation
  - Linux: `sudo git-ai bg start` → verify de-escalation
  - Windows: Run as Administrator → verify linked token respawn
  - All platforms: Simulate stale lock → verify auto-recovery
  - All platforms: Simulate privilege mismatch → verify actionable error

## Edge Cases

- **Docker containers running as root**: Typically no `SUDO_UID`. Users should set `GIT_AI_DAEMON_ALLOW_ROOT=true` or add to config feature_flags.
- **CI systems**: Same as Docker — use `GIT_AI_DAEMON_ALLOW_ROOT=true` env var.
- **`su` without SUDO_UID**: De-escalation won't trigger (no env var to read). Layers 2-4 handle this: relaxed perms allow connection, and if that fails, actionable error explains the fix.
- **Windows service context**: Not a split token, so no linked token. `daemon_allow_root` feature flag required. Services typically run under a dedicated service account anyway.
- **Race condition on lock cleanup (Layer 4)**: Between checking PID death and removing lock, another process could start. Mitigate by re-attempting lock acquisition immediately after cleanup — if it fails again, another process won the race (which is fine).
