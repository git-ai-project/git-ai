use crate::config::Config;

#[derive(Debug, PartialEq)]
pub enum PrivilegeAction {
    Continue,
    Refuse(String),
}

#[derive(Debug, PartialEq)]
pub enum PidStatus {
    Dead,
    Alive,
    AliveButInaccessible,
}

#[cfg(unix)]
pub fn check_and_deescalate_privileges() -> PrivilegeAction {
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        return PrivilegeAction::Continue;
    }

    // We are running as root. Try to de-escalate via SUDO_UID/SUDO_GID.
    let sudo_uid = std::env::var("SUDO_UID")
        .ok()
        .and_then(|v| v.parse::<u32>().ok());
    let sudo_gid = std::env::var("SUDO_GID")
        .ok()
        .and_then(|v| v.parse::<u32>().ok());

    if let (Some(uid), Some(gid)) = (sudo_uid, sudo_gid)
        && uid != 0
    {
        eprintln!(
            "[git-ai] dropping root privileges to uid={} gid={}",
            uid, gid
        );

        unsafe {
            let gid_c = gid as libc::gid_t;
            if libc::setgroups(1, &gid_c) != 0 {
                return PrivilegeAction::Refuse(format!(
                    "failed to setgroups: {}",
                    std::io::Error::last_os_error()
                ));
            }
            if libc::setgid(gid_c) != 0 {
                return PrivilegeAction::Refuse(format!(
                    "failed to setgid: {}",
                    std::io::Error::last_os_error()
                ));
            }
            if libc::setuid(uid as libc::uid_t) != 0 {
                return PrivilegeAction::Refuse(format!(
                    "failed to setuid: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }

        // Clear SUDO_* env vars now that we've dropped privileges
        unsafe {
            std::env::remove_var("SUDO_UID");
            std::env::remove_var("SUDO_GID");
            std::env::remove_var("SUDO_USER");
            std::env::remove_var("SUDO_COMMAND");
        }

        return PrivilegeAction::Continue;
    }

    // True root (SUDO_UID=0, unparseable, or absent) — check feature flag
    let allow_root = Config::get().get_feature_flags().allow_root;
    if allow_root {
        eprintln!("[git-ai] WARNING: running as root with allow_root=true");
        PrivilegeAction::Continue
    } else {
        PrivilegeAction::Refuse(
            "git-ai daemon refuses to run as root. \
             Set GIT_AI_ALLOW_ROOT=true to override."
                .to_string(),
        )
    }
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
        _ => PidStatus::Dead,
    }
}

#[cfg(windows)]
pub fn check_and_deescalate_privileges() -> PrivilegeAction {
    if !is_elevated_windows() {
        return PrivilegeAction::Continue;
    }

    // If --respawned is present, we already tried de-escalation
    if std::env::args().any(|arg| arg == "--respawned") {
        let allow_root = Config::get().get_feature_flags().allow_root;
        if allow_root {
            eprintln!("[git-ai] WARNING: running elevated with allow_root=true");
            return PrivilegeAction::Continue;
        } else {
            return PrivilegeAction::Refuse(
                "git-ai daemon refuses to run elevated. \
                 Set GIT_AI_ALLOW_ROOT=true to override."
                    .to_string(),
            );
        }
    }

    // Try to respawn de-escalated
    match respawn_deescalated_windows() {
        Ok(()) => {
            // Child process spawned successfully, parent should exit
            std::process::exit(0);
        }
        Err(_) => {
            let allow_root = Config::get().get_feature_flags().allow_root;
            if allow_root {
                eprintln!(
                    "[git-ai] WARNING: running elevated with allow_root=true \
                     (de-escalation failed)"
                );
                PrivilegeAction::Continue
            } else {
                PrivilegeAction::Refuse(
                    "git-ai daemon refuses to run elevated and de-escalation failed. \
                     Set GIT_AI_ALLOW_ROOT=true to override."
                        .to_string(),
                )
            }
        }
    }
}

#[cfg(windows)]
pub fn check_pid_status(pid: u32) -> PidStatus {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle != 0 {
        unsafe { CloseHandle(handle) };
        PidStatus::Alive
    } else {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        if err == ERROR_ACCESS_DENIED {
            PidStatus::AliveButInaccessible
        } else {
            PidStatus::Dead
        }
    }
}

#[cfg(windows)]
fn is_elevated_windows() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token_handle = 0;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle) == 0 {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut return_length = 0u32;
        let result = GetTokenInformation(
            token_handle,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut return_length,
        );
        CloseHandle(token_handle);

        result != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(windows)]
fn respawn_deescalated_windows() -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_LINKED_TOKEN, TOKEN_QUERY, TokenLinkedToken,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessWithTokenW, GetCurrentProcess, LOGON_WITH_PROFILE, OpenProcessToken,
        PROCESS_INFORMATION, STARTUPINFOW,
    };

    unsafe {
        let mut token_handle = 0;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle) == 0 {
            return Err("failed to open process token".to_string());
        }

        let mut linked_token = TOKEN_LINKED_TOKEN { LinkedToken: 0 };
        let mut return_length = 0u32;
        let result = GetTokenInformation(
            token_handle,
            TokenLinkedToken,
            &mut linked_token as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_LINKED_TOKEN>() as u32,
            &mut return_length,
        );
        CloseHandle(token_handle);

        if result == 0 || linked_token.LinkedToken == 0 {
            return Err("failed to get linked token".to_string());
        }

        let exe_path =
            std::env::current_exe().map_err(|e| format!("failed to get current exe: {}", e))?;
        let args: Vec<String> = std::env::args().collect();
        let mut cmd_line = format!("\"{}\"", exe_path.display());
        for arg in &args[1..] {
            cmd_line.push_str(&format!(" \"{}\"", arg));
        }
        cmd_line.push_str(" \"--respawned\"");

        let cmd_line_wide: Vec<u16> = cmd_line.encode_utf16().chain(std::iter::once(0)).collect();

        let mut startup_info: STARTUPINFOW = std::mem::zeroed();
        startup_info.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut process_info: PROCESS_INFORMATION = std::mem::zeroed();

        let create_result = CreateProcessWithTokenW(
            linked_token.LinkedToken,
            LOGON_WITH_PROFILE,
            std::ptr::null(),
            cmd_line_wide.as_ptr() as *mut _,
            0,
            std::ptr::null(),
            std::ptr::null(),
            &startup_info,
            &mut process_info,
        );

        CloseHandle(linked_token.LinkedToken);

        if create_result == 0 {
            return Err(format!(
                "CreateProcessWithTokenW failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        CloseHandle(process_info.hProcess);
        CloseHandle(process_info.hThread);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_pid_status_dead_process() {
        // PID 4_000_000_000 is impossibly high and guaranteed not running
        assert_eq!(check_pid_status(4_000_000_000), PidStatus::Dead);
    }

    #[test]
    fn check_pid_status_alive_process() {
        // Current process is definitely alive
        let pid = std::process::id();
        assert_eq!(check_pid_status(pid), PidStatus::Alive);
    }

    #[cfg(unix)]
    #[test]
    fn privilege_check_not_elevated() {
        // Tests run as normal user, should return Continue
        // (unless tests are running as root, which would be unusual)
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(check_and_deescalate_privileges(), PrivilegeAction::Continue);
        }
    }
}
