# Daemon Privilege Mismatch Prevention - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent the daemon from entering an unrecoverable broken state when started by a privileged user and accessed by a non-privileged user.

**Architecture:** Four-layer defense: (1) privilege de-escalation at daemon startup, (2) relaxed socket/lock permissions, (3) privilege mismatch detection with actionable errors, (4) dead process auto-recovery. A new `daemon_allow_root` feature flag controls whether true-root operation is permitted.

**Tech Stack:** Rust, `libc` crate (Unix privilege ops), `windows-sys` crate (Windows token APIs), existing `DaemonConfig`/`DaemonLock` infrastructure.

---

## File Structure

| File | Responsibility |
|------|---------------|
| `src/privilege.rs` (create) | Cross-platform privilege detection and de-escalation logic |
| `src/feature_flags.rs` (modify) | Add `daemon_allow_root` feature flag |
| `src/daemon.rs` (modify:8429-8435) | Call privilege check before lock acquisition in `run_daemon()` |
| `src/daemon.rs` (modify:3456-3465) | Relax socket permissions from 0o600 to 0o660 |
| `src/commands/daemon.rs` (modify:103-114,294-308) | Replace opaque "lock held" error with Layer 3+4 logic |
| `src/commands/daemon.rs` (modify:337-413) | Pass `--respawned` flag on Windows de-escalation respawn |
| `Cargo.toml` (modify:47-49) | Add `windows-sys` as runtime dependency with token features |
| `tests/repos/daemon_privilege_test.rs` (create) | Integration tests for privilege detection and stale lock recovery |

---

### Task 1: Add `daemon_allow_root` Feature Flag

**Files:**
- Modify: `src/feature_flags.rs:80-87`

- [ ] **Step 1: Add the flag to `define_feature_flags!`**

In `src/feature_flags.rs`, add the new flag after `transcript_sweep`:

```rust
define_feature_flags!(
    rewrite_stash: rewrite_stash, debug = true, release = true,
    auth_keyring: auth_keyring, debug = false, release = false,
    git_hooks_enabled: git_hooks_enabled, debug = false, release = false,
    git_hooks_externally_managed: git_hooks_externally_managed, debug = false, release = false,
    transcript_streaming: transcript_streaming, debug = true, release = true,
    transcript_sweep: transcript_sweep, debug = true, release = false,
    daemon_allow_root: daemon_allow_root, debug = false, release = false,
);
```

- [ ] **Step 2: Run tests to verify no regressions**

Run: `task test TEST_FILTER=test_default_feature_flags`
Expected: PASS (the test checks debug defaults, new flag defaults to false)

- [ ] **Step 3: Commit**

```bash
git add src/feature_flags.rs
git commit -m "feat: add daemon_allow_root feature flag (default false)"
```

---

### Task 2: Create `src/privilege.rs` — Unix Privilege Detection & De-escalation

**Files:**
- Create: `src/privilege.rs`
- Modify: `src/main.rs` (add `mod privilege;`)

- [ ] **Step 1: Create `src/privilege.rs` with Unix implementation**

```rust
use crate::config::Config;

#[derive(Debug, PartialEq)]
pub enum PrivilegeAction {
    Continue,
    Refuse(String),
}

#[cfg(unix)]
pub fn check_and_deescalate_privileges() -> PrivilegeAction {
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        return PrivilegeAction::Continue;
    }

    // We're running as root. Try to de-escalate.
    if let Some(action) = try_deescalate_unix() {
        return action;
    }

    // True root (no SUDO_UID). Check feature flag.
    if Config::get().get_feature_flags().daemon_allow_root {
        eprintln!("[git-ai] warning: daemon running as root (daemon_allow_root=true)");
        return PrivilegeAction::Continue;
    }

    PrivilegeAction::Refuse(
        "Refusing to start daemon as root without a real user to de-escalate to. \
         Set GIT_AI_DAEMON_ALLOW_ROOT=true or add daemon_allow_root to config feature_flags to override."
            .to_string(),
    )
}

#[cfg(unix)]
fn try_deescalate_unix() -> Option<PrivilegeAction> {
    let sudo_uid_str = std::env::var("SUDO_UID").ok()?;
    let sudo_gid_str = std::env::var("SUDO_GID").ok()?;

    let uid: libc::uid_t = sudo_uid_str.parse().ok()?;
    let gid: libc::gid_t = sudo_gid_str.parse().ok()?;

    if uid == 0 {
        // SUDO_UID=0 means root sudo'd to root — nothing to de-escalate to
        return None;
    }

    eprintln!(
        "[git-ai] dropping root privileges to uid={} gid={}",
        uid, gid
    );

    // Order matters: setgroups before setgid before setuid
    unsafe {
        if libc::setgroups(1, &gid) != 0 {
            let err = std::io::Error::last_os_error();
            return Some(PrivilegeAction::Refuse(format!(
                "failed to setgroups during privilege de-escalation: {}",
                err
            )));
        }
        if libc::setgid(gid) != 0 {
            let err = std::io::Error::last_os_error();
            return Some(PrivilegeAction::Refuse(format!(
                "failed to setgid({}) during privilege de-escalation: {}",
                gid, err
            )));
        }
        if libc::setuid(uid) != 0 {
            let err = std::io::Error::last_os_error();
            return Some(PrivilegeAction::Refuse(format!(
                "failed to setuid({}) during privilege de-escalation: {}",
                uid, err
            )));
        }
    }

    // Clear SUDO_* env vars so child processes don't think they're still elevated
    std::env::remove_var("SUDO_UID");
    std::env::remove_var("SUDO_GID");
    std::env::remove_var("SUDO_USER");
    std::env::remove_var("SUDO_COMMAND");

    Some(PrivilegeAction::Continue)
}

#[cfg(windows)]
pub fn check_and_deescalate_privileges() -> PrivilegeAction {
    if !is_elevated_windows() {
        return PrivilegeAction::Continue;
    }

    // We're elevated. Try to respawn with linked token.
    // If --respawned flag is set, we already tried — don't loop.
    if std::env::args().any(|a| a == "--respawned") {
        if Config::get().get_feature_flags().daemon_allow_root {
            eprintln!("[git-ai] warning: daemon running with administrator privileges (daemon_allow_root=true)");
            return PrivilegeAction::Continue;
        }
        return PrivilegeAction::Refuse(
            "Refusing to start daemon with administrator privileges. \
             Set GIT_AI_DAEMON_ALLOW_ROOT=true or add daemon_allow_root to config feature_flags to override."
                .to_string(),
        );
    }

    match respawn_deescalated_windows() {
        Ok(()) => {
            // Parent exits — child will take over
            std::process::exit(0);
        }
        Err(e) => {
            // No linked token available (true admin, not UAC split)
            if Config::get().get_feature_flags().daemon_allow_root {
                eprintln!(
                    "[git-ai] warning: could not de-escalate ({}), running as administrator (daemon_allow_root=true)",
                    e
                );
                return PrivilegeAction::Continue;
            }
            PrivilegeAction::Refuse(format!(
                "Refusing to start daemon with administrator privileges (de-escalation failed: {}). \
                 Set GIT_AI_DAEMON_ALLOW_ROOT=true or add daemon_allow_root to config feature_flags to override.",
                e
            ))
        }
    }
}

#[cfg(windows)]
fn is_elevated_windows() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = std::mem::zeroed();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }

        let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
        let mut size = 0u32;
        let result = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        );
        CloseHandle(token);

        result != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(windows)]
fn respawn_deescalated_windows() -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenLinkedToken, TOKEN_LINKED_TOKEN, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessWithTokenW, GetCurrentProcess, OpenProcessToken,
        LOGON_WITH_PROFILE, PROCESS_INFORMATION, STARTUPINFOW,
    };

    unsafe {
        let mut token = std::mem::zeroed();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err("OpenProcessToken failed".to_string());
        }

        let mut linked: TOKEN_LINKED_TOKEN = std::mem::zeroed();
        let mut size = 0u32;
        let result = GetTokenInformation(
            token,
            TokenLinkedToken,
            &mut linked as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_LINKED_TOKEN>() as u32,
            &mut size,
        );
        CloseHandle(token);

        if result == 0 {
            return Err("no linked token available (not a UAC split-token process)".to_string());
        }

        let linked_token = linked.LinkedToken;

        // Build command line: current exe + "bg run --respawned"
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let cmd_line: Vec<u16> = format!("\"{}\" bg run --respawned", exe.display())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();

        let result = CreateProcessWithTokenW(
            linked_token,
            LOGON_WITH_PROFILE,
            std::ptr::null(),
            cmd_line.as_ptr() as *mut _,
            0, // creation flags
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        );

        CloseHandle(linked_token);

        if result == 0 {
            return Err(format!(
                "CreateProcessWithTokenW failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
        Ok(())
    }
}

/// Check if a given PID is alive, dead, or alive-but-inaccessible (privilege mismatch).
#[derive(Debug, PartialEq)]
pub enum PidStatus {
    Dead,
    Alive,
    AliveButInaccessible,
}

#[cfg(unix)]
pub fn check_pid_status(pid: u32) -> PidStatus {
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return PidStatus::Alive;
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::ESRCH) => PidStatus::Dead,
        Some(libc::EPERM) => PidStatus::AliveButInaccessible,
        _ => PidStatus::Dead, // treat unexpected errors as dead
    }
}

#[cfg(windows)]
pub fn check_pid_status(pid: u32) -> PidStatus {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(5) => PidStatus::AliveButInaccessible, // ERROR_ACCESS_DENIED
                _ => PidStatus::Dead,
            }
        } else {
            CloseHandle(handle);
            PidStatus::Alive
        }
    }
}
```

- [ ] **Step 2: Register the module in `src/main.rs`**

Add `mod privilege;` near the other module declarations in `src/main.rs`.

Find the existing `mod` declarations and add:

```rust
mod privilege;
```

- [ ] **Step 3: Verify it compiles**

Run: `task build`
Expected: Successful compilation (note: Windows-specific code won't compile on macOS without `windows-sys` dep — that's expected, handled in Task 5)

- [ ] **Step 4: Commit**

```bash
git add src/privilege.rs src/main.rs
git commit -m "feat: add privilege detection and de-escalation module"
```

---

### Task 3: Add `windows-sys` as Runtime Dependency

**Files:**
- Modify: `Cargo.toml:47-49`

- [ ] **Step 1: Add `windows-sys` to Windows runtime dependencies**

In `Cargo.toml`, change the `[target.'cfg(windows)'.dependencies]` section:

```toml
[target.'cfg(windows)'.dependencies]
named_pipe = "0.4.1"
winreg = "0.55"
windows-sys = { version = "0.61", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_System_Threading",
] }
```

- [ ] **Step 2: Remove duplicate from dev-dependencies**

The existing `[target.'cfg(windows)'.dev-dependencies]` entry for `windows-sys` should be updated to only keep features NOT already in the runtime dep:

```toml
[target.'cfg(windows)'.dev-dependencies]
windows-sys = { version = "0.61", features = ["Win32_System_JobObjects"] }
```

- [ ] **Step 3: Verify it compiles**

Run: `task build`
Expected: Successful compilation

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: add windows-sys as runtime dependency for token APIs"
```

---

### Task 4: Integrate Privilege Check into `run_daemon()`

**Files:**
- Modify: `src/daemon.rs:8429-8435`

- [ ] **Step 1: Add privilege check before lock acquisition**

In `src/daemon.rs`, in `run_daemon()`, insert the privilege check after `sanitize_git_env_for_daemon()` and before `config.ensure_parent_dirs()`:

```rust
pub(crate) async fn run_daemon(config: DaemonConfig) -> Result<DaemonExitAction, GitAiError> {
    sanitize_git_env_for_daemon();
    disable_trace2_for_daemon_process();

    match crate::privilege::check_and_deescalate_privileges() {
        crate::privilege::PrivilegeAction::Continue => {}
        crate::privilege::PrivilegeAction::Refuse(msg) => {
            return Err(GitAiError::Generic(msg));
        }
    }

    config.ensure_parent_dirs()?;
    let _lock = DaemonLock::acquire(&config.lock_path)?;
    // ... rest unchanged
```

- [ ] **Step 2: Verify it compiles**

Run: `task build`
Expected: Successful compilation

- [ ] **Step 3: Commit**

```bash
git add src/daemon.rs
git commit -m "feat: integrate privilege de-escalation into daemon startup"
```

---

### Task 5: Relax Socket Permissions (Layer 2)

**Files:**
- Modify: `src/daemon.rs:3456-3465`

- [ ] **Step 1: Change socket permissions from 0o600 to 0o660**

In `src/daemon.rs`, modify `set_socket_owner_only()`:

```rust
#[cfg(not(windows))]
fn set_socket_owner_only(path: &Path) -> Result<(), GitAiError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 0o660: owner + group read/write. Since ~/.git-ai/ is user-scoped (0700),
        // this allows same-user processes at different privilege levels to connect.
        fs::set_permissions(path, fs::Permissions::from_mode(0o660))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
```

- [ ] **Step 2: Verify it compiles**

Run: `task build`
Expected: Successful compilation

- [ ] **Step 3: Commit**

```bash
git add src/daemon.rs
git commit -m "feat: relax daemon socket permissions to 0o660 for cross-privilege access"
```

---

### Task 6: Implement Layer 3+4 — Privilege Mismatch Detection & Dead Process Recovery

**Files:**
- Modify: `src/commands/daemon.rs:103-114` (in `ensure_daemon_running_attached`)
- Modify: `src/commands/daemon.rs:294-308` (in `start_daemon_detached_with_config`)

- [ ] **Step 1: Add a helper function for enhanced lock-blocked diagnostics**

Add this function in `src/commands/daemon.rs` (after the existing `daemon_startup_is_blocked` function around line 268):

```rust
/// When the lock is held but the daemon is not connectable, determine why and
/// either auto-recover (stale lock from dead process) or provide an actionable error.
fn diagnose_blocked_daemon(config: &DaemonConfig) -> Result<(), String> {
    // Try to read PID from metadata
    let pid = match crate::daemon::read_daemon_pid(config) {
        Ok(pid) => pid,
        Err(_) => {
            // No PID metadata — can't diagnose further. Maybe stale lock from crash.
            // Try force-removing the lock file and retrying.
            if force_remove_stale_files(config) {
                return Ok(()); // Caller should retry
            }
            return Err(format!(
                "daemon startup blocked: lock held at {} (no PID metadata found, manual removal may be required)",
                config.lock_path.display()
            ));
        }
    };

    match crate::privilege::check_pid_status(pid) {
        crate::privilege::PidStatus::Dead => {
            // Process is dead but lock persists (crash/reboot). Clean up and retry.
            eprintln!(
                "[git-ai] cleaning up stale daemon files from dead process (pid {})",
                pid
            );
            if force_remove_stale_files(config) {
                return Ok(()); // Caller should retry
            }
            Err(format!(
                "daemon startup blocked: lock held at {} by dead process (pid {}), but cleanup failed — manual removal required",
                config.lock_path.display(),
                pid
            ))
        }
        crate::privilege::PidStatus::AliveButInaccessible => {
            // Privilege mismatch — process is alive but we can't signal it
            #[cfg(unix)]
            let hint = format!(
                "Daemon (pid {}) is running as a different user. To fix:\n  sudo kill {} && rm -f \"{}\"",
                pid, pid, config.lock_path.display()
            );
            #[cfg(windows)]
            let hint = format!(
                "Daemon (pid {}) is running with elevated privileges. Open an Administrator terminal and run:\n  git-ai bg stop",
                pid
            );
            Err(hint)
        }
        crate::privilege::PidStatus::Alive => {
            // Process is alive and accessible but sockets aren't responding.
            // Could be starting up, or in a bad state.
            Err(format!(
                "daemon startup blocked: lock held at {} by process {} (alive but not responding on sockets)",
                config.lock_path.display(),
                pid
            ))
        }
    }
}

fn force_remove_stale_files(config: &DaemonConfig) -> bool {
    let mut ok = true;
    if config.lock_path.exists() {
        if std::fs::remove_file(&config.lock_path).is_err() {
            ok = false;
        }
    }
    // Also remove stale sockets
    let _ = std::fs::remove_file(&config.control_socket_path);
    let _ = std::fs::remove_file(&config.trace_socket_path);
    ok
}
```

- [ ] **Step 2: Replace the "lock held" error in `ensure_daemon_running_attached`**

In `src/commands/daemon.rs`, replace the block at lines 109-114:

Old code:
```rust
    if daemon_startup_is_blocked(&config) {
        return Err(format!(
            "daemon startup blocked: lock held at {}",
            config.lock_path.display()
        ));
    }
```

New code:
```rust
    if daemon_startup_is_blocked(&config) {
        diagnose_blocked_daemon(&config)?;
        // diagnose_blocked_daemon returned Ok — stale files cleaned up, retry
        if daemon_startup_is_blocked(&config) {
            return Err(format!(
                "daemon startup blocked: lock held at {} (cleanup succeeded but lock still held)",
                config.lock_path.display()
            ));
        }
    }
```

- [ ] **Step 3: Replace the "lock held" error in `start_daemon_detached_with_config`**

In `src/commands/daemon.rs`, replace the block at lines 303-308:

Old code:
```rust
    if daemon_startup_is_blocked(&config) {
        return Err(format!(
            "daemon startup blocked: lock held at {}",
            config.lock_path.display()
        ));
    }
```

New code:
```rust
    if daemon_startup_is_blocked(&config) {
        diagnose_blocked_daemon(&config)?;
        // diagnose_blocked_daemon returned Ok — stale files cleaned up, retry
        if daemon_startup_is_blocked(&config) {
            return Err(format!(
                "daemon startup blocked: lock held at {} (cleanup succeeded but lock still held)",
                config.lock_path.display()
            ));
        }
    }
```

- [ ] **Step 4: Verify it compiles**

Run: `task build`
Expected: Successful compilation

- [ ] **Step 5: Commit**

```bash
git add src/commands/daemon.rs
git commit -m "feat: add privilege mismatch detection and dead process auto-recovery"
```

---

### Task 7: Pass `--respawned` on Windows De-escalation Respawn

**Files:**
- Modify: `src/commands/daemon.rs:174-178` (in `handle_run`)

- [ ] **Step 1: Accept `--respawned` flag in `handle_run` without error**

The `handle_run` function currently only checks for `--mode`. The `--respawned` flag is already consumed by `privilege.rs` via `std::env::args()`, so no additional handling is needed in `handle_run` itself. However, we need to make sure it doesn't trigger "unknown flag" errors if any such validation exists.

Verify by reading `handle_run` — it only checks `--mode` with `has_flag`. The `--respawned` flag is passed through `std::env::args()` in `privilege.rs` and doesn't need explicit handling in daemon command parsing.

No code change needed for this step. The `--respawned` flag is already part of the command line args that `privilege.rs` checks via `std::env::args().any(|a| a == "--respawned")`.

- [ ] **Step 2: Verify the respawn command line in `privilege.rs` is correct**

In `respawn_deescalated_windows()`, the command line is:
```rust
let cmd_line: Vec<u16> = format!("\"{}\" bg run --respawned", exe.display())
```

This matches what `handle_daemon` dispatches: `args[0] == "run"` → `handle_run(&args[1..])`. The `--respawned` arg ends up in `args` for `handle_run`, which ignores unknown flags (it only rejects `--mode`).

No additional change needed. Mark complete.

- [ ] **Step 3: Commit (skip if no changes)**

No changes needed — the design already handles this correctly in the `privilege.rs` module.

---

### Task 8: Integration Tests

**Files:**
- Create: `tests/repos/daemon_privilege_test.rs`

- [ ] **Step 1: Write test for stale lock auto-recovery (Layer 4)**

This test simulates a daemon crash leaving a stale lock file, verifying that the next startup cleans up and succeeds.

Create `tests/repos/daemon_privilege_test.rs`:

```rust
use crate::test_repo::TestRepo;
use std::fs;

#[test]
fn test_stale_lock_auto_recovery() {
    let repo = TestRepo::new();

    // Start daemon normally, then stop it
    repo.git_ai(&["bg", "start"]).unwrap();
    repo.git_ai(&["bg", "shutdown"]).unwrap();

    // Simulate a stale lock by creating the lock file manually
    // (as if the daemon crashed without cleanup)
    let internal_dir = repo.home_dir().join(".git-ai").join("internal").join("daemon");
    fs::create_dir_all(&internal_dir).unwrap();
    let lock_path = internal_dir.join("daemon.lock");
    fs::write(&lock_path, "stale").unwrap();

    // Also write a fake PID metadata pointing to a dead process
    let pid_path = internal_dir.join("daemon.pid.json");
    fs::write(
        &pid_path,
        r#"{"pid": 999999999, "started_at_ns": 0}"#,
    )
    .unwrap();

    // Starting the daemon should auto-recover by cleaning up stale files
    let result = repo.git_ai(&["bg", "start"]);
    assert!(
        result.is_ok(),
        "daemon start should succeed after stale lock cleanup, got: {:?}",
        result
    );

    // Verify daemon is actually running now
    repo.git_ai(&["bg", "shutdown"]).unwrap();
}
```

- [ ] **Step 2: Write test for PID status detection**

Add to the same file:

```rust
#[test]
fn test_check_pid_status_dead_process() {
    use git_ai::privilege::{check_pid_status, PidStatus};

    // PID 999999999 is extremely unlikely to be running
    let status = check_pid_status(999999999);
    assert_eq!(status, PidStatus::Dead);
}

#[test]
fn test_check_pid_status_alive_process() {
    use git_ai::privilege::{check_pid_status, PidStatus};

    // Current process PID is definitely alive
    let pid = std::process::id();
    let status = check_pid_status(pid);
    assert_eq!(status, PidStatus::Alive);
}
```

- [ ] **Step 3: Write test for privilege check when not elevated**

```rust
#[test]
fn test_privilege_check_not_elevated() {
    use git_ai::privilege::{check_and_deescalate_privileges, PrivilegeAction};

    // In normal test execution (not root), should return Continue
    let action = check_and_deescalate_privileges();
    assert_eq!(action, PrivilegeAction::Continue);
}
```

- [ ] **Step 4: Register the test module**

Add `mod daemon_privilege_test;` to `tests/repos/mod.rs`.

- [ ] **Step 5: Make necessary types/functions public**

In `src/privilege.rs`, ensure `check_pid_status`, `PidStatus`, `check_and_deescalate_privileges`, and `PrivilegeAction` are `pub`. In `src/main.rs` (or `src/lib.rs`), ensure `pub mod privilege;` is accessible from tests.

- [ ] **Step 6: Run tests**

Run: `task test TEST_FILTER=daemon_privilege`
Expected: All tests PASS

- [ ] **Step 7: Commit**

```bash
git add tests/repos/daemon_privilege_test.rs tests/repos/mod.rs src/privilege.rs
git commit -m "test: add integration tests for daemon privilege handling"
```

---

### Task 9: Lint, Format, and Final Verification

**Files:**
- All modified files

- [ ] **Step 1: Run formatter**

Run: `task fmt`
Expected: No errors (may format some files)

- [ ] **Step 2: Run linter**

Run: `task lint`
Expected: No warnings or errors

- [ ] **Step 3: Run full test suite**

Run: `task test`
Expected: All tests PASS

- [ ] **Step 4: Commit any formatting fixes**

```bash
git add -A
git commit -m "chore: fmt and lint cleanup"
```

(Skip if no changes.)
