use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{self, Command, Stdio};

/// Entry point for `git-ai doctor`.
pub fn handle_doctor(_args: &[String]) {
    let mut all_passed = true;

    // 1. Binary exists and is executable
    all_passed &= check_binary_installed();

    // 2. Daemon running (PID file + process alive)
    all_passed &= check_daemon_running();

    // 3. Control socket responsive
    all_passed &= check_control_socket();

    // 4. Agent hook configs
    all_passed &= check_agent_hooks();

    // 5. Git notes namespace accessible
    all_passed &= check_git_notes();

    // 6. Config file valid
    all_passed &= check_config_valid();

    // 7. Working log directory exists
    all_passed &= check_working_log_dir();

    if !all_passed {
        process::exit(1);
    }
}

fn pass(msg: &str) {
    println!("\u{2713} {}", msg);
}

fn fail(msg: &str) {
    println!("\u{2717} {}", msg);
}

fn home_dir() -> Option<PathBuf> {
    env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| env::var("USERPROFILE").ok().map(PathBuf::from))
}

fn check_binary_installed() -> bool {
    let home = match home_dir() {
        Some(h) => h,
        None => {
            fail("Binary check — cannot determine HOME directory");
            return false;
        }
    };

    let bin_path = home.join(".git-ai/bin/git-ai");
    if !bin_path.exists() {
        fail(&format!(
            "Binary missing at {}",
            bin_path.display()
        ));
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(&bin_path) {
            let mode = meta.permissions().mode();
            if mode & 0o111 == 0 {
                fail(&format!(
                    "Binary not executable at {}",
                    bin_path.display()
                ));
                return false;
            }
        }
    }

    pass(&format!("Binary installed at {}", bin_path.display()));
    true
}

fn check_daemon_running() -> bool {
    let home = match home_dir() {
        Some(h) => h,
        None => {
            fail("Daemon check — cannot determine HOME directory");
            return false;
        }
    };

    let pid_path = home
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("daemon.pid.json");

    if !pid_path.exists() {
        fail("Daemon not running (no PID file)");
        return false;
    }

    let content = match fs::read_to_string(&pid_path) {
        Ok(c) => c,
        Err(_) => {
            fail("Daemon PID file unreadable");
            return false;
        }
    };

    let pid = match extract_pid_from_json(&content) {
        Some(p) => p,
        None => {
            fail("Daemon PID file malformed");
            return false;
        }
    };

    #[cfg(unix)]
    {
        let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        if !alive {
            fail(&format!("Daemon not running (stale PID {})", pid));
            return false;
        }
        pass(&format!("Daemon running (PID {})", pid));
        return true;
    }

    #[cfg(not(unix))]
    {
        // On non-unix, we just check the PID file exists
        pass(&format!("Daemon PID file exists (PID {})", pid));
        true
    }
}

fn check_control_socket() -> bool {
    #[cfg(unix)]
    {
        use std::io::{BufRead, Write};
        use std::os::unix::net::UnixStream;
        use std::time::Duration;

        let home = match home_dir() {
            Some(h) => h,
            None => {
                fail("Control socket check — cannot determine HOME directory");
                return false;
            }
        };

        let base_dir = home
            .join(".git-ai")
            .join("internal")
            .join("daemon");

        let sock_path = resolve_socket_path(&base_dir, "control");

        if !sock_path.exists() {
            fail(&format!(
                "Control socket missing at {}",
                sock_path.display()
            ));
            return false;
        }

        match UnixStream::connect(&sock_path) {
            Ok(mut stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

                if writeln!(stream, r#"{{"type":"ping"}}"#).is_err() {
                    fail("Control socket — failed to write");
                    return false;
                }
                if stream.flush().is_err() {
                    fail("Control socket — failed to flush");
                    return false;
                }

                let reader = std::io::BufReader::new(&stream);
                match reader.lines().next() {
                    Some(Ok(_)) => {
                        pass("Control socket responsive");
                        true
                    }
                    _ => {
                        // Even if no response, connection succeeded
                        pass("Control socket connectable");
                        true
                    }
                }
            }
            Err(e) => {
                fail(&format!("Control socket not responsive — {}", e));
                false
            }
        }
    }

    #[cfg(not(unix))]
    {
        pass("Control socket check skipped (non-unix)");
        true
    }
}

fn check_agent_hooks() -> bool {
    let statuses = git_ai::mdm::status();
    let mut all_ok = true;

    for agent in &statuses {
        if !agent.detected {
            continue;
        }

        match agent.hooks_installed {
            Some(true) => {
                pass(&format!("{} hooks installed", agent.name));
            }
            Some(false) => {
                fail(&format!(
                    "{} hooks missing — run `git-ai install`",
                    agent.name
                ));
                all_ok = false;
            }
            None => {
                // Not installable or no config path — skip silently for detected agents
                // that don't support auto-install
                if agent.installable {
                    fail(&format!(
                        "{} hooks not configured — run `git-ai install`",
                        agent.name
                    ));
                    all_ok = false;
                }
            }
        }
    }

    all_ok
}

fn check_git_notes() -> bool {
    let result = Command::new("/usr/bin/git")
        .args(["notes", "--ref=ai", "list"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match result {
        Ok(status) if status.success() => {
            pass("Git notes namespace accessible");
            true
        }
        Ok(_) => {
            // Non-zero exit can mean empty namespace (which is fine) or not in a repo
            // Try to distinguish: if we're not in a repo at all, that's a warning
            let in_repo = Command::new("/usr/bin/git")
                .args(["rev-parse", "--git-dir"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());

            if in_repo {
                pass("Git notes namespace accessible (empty)");
                true
            } else {
                pass("Git notes check skipped (not in a git repository)");
                true
            }
        }
        Err(_) => {
            fail("Git notes check failed — git not found");
            false
        }
    }
}

fn check_config_valid() -> bool {
    let home = match home_dir() {
        Some(h) => h,
        None => {
            fail("Config check — cannot determine HOME directory");
            return false;
        }
    };

    let config_path = home.join(".git-ai/config.json");
    if !config_path.exists() {
        pass("Config file absent (using defaults)");
        return true;
    }

    let content = match fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(e) => {
            fail(&format!("Config file unreadable — {}", e));
            return false;
        }
    };

    if content.trim().is_empty() {
        pass("Config file empty (using defaults)");
        return true;
    }

    match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(_) => {
            pass("Config valid");
            true
        }
        Err(e) => {
            fail(&format!("Config file invalid JSON — {}", e));
            false
        }
    }
}

fn check_working_log_dir() -> bool {
    // Working logs are stored in .git/ai/working_logs/ inside a repository.
    // If we're in a repo, check that the ai directory is accessible.
    let git_dir = Command::new("/usr/bin/git")
        .args(["rev-parse", "--git-dir"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    match git_dir {
        Ok(output) if output.status.success() => {
            let git_dir_path =
                PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
            let ai_dir = git_dir_path.join("ai");
            if ai_dir.is_dir() {
                pass("Working log directory exists");
            } else {
                pass("Working log directory will be created on first checkpoint");
            }
            true
        }
        _ => {
            pass("Working log check skipped (not in a git repository)");
            true
        }
    }
}

/// Extract "pid" value from a minimal JSON object like {"pid":1234,...}
fn extract_pid_from_json(json: &str) -> Option<u32> {
    let pattern = "\"pid\":";
    let idx = json.find(pattern)?;
    let after = json[idx + pattern.len()..].trim_start();
    let end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    if end == 0 {
        return None;
    }
    after[..end].parse().ok()
}

/// Resolve socket path, matching DaemonPaths logic.
#[cfg(unix)]
fn resolve_socket_path(base_dir: &std::path::Path, name: &str) -> PathBuf {
    let candidate = base_dir.join(format!("{}.sock", name));
    let candidate_str = candidate.to_string_lossy();

    if candidate_str.len() >= 100 {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(base_dir.to_string_lossy().as_bytes());
        let hash = hasher.finalize();
        let short_hash: String = hash[..8].iter().map(|b| format!("{:02x}", b)).collect();
        let dir = PathBuf::from(format!("/tmp/git-ai-d-{}", short_hash));
        dir.join(format!("{}.sock", name))
    } else {
        candidate
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_pid_from_json() {
        assert_eq!(
            extract_pid_from_json(r#"{"pid":12345,"started_at":"2024-01-01"}"#),
            Some(12345)
        );
        assert_eq!(
            extract_pid_from_json(r#"{"pid": 999}"#),
            Some(999)
        );
        assert_eq!(extract_pid_from_json(r#"{}"#), None);
        assert_eq!(extract_pid_from_json(r#"{"pid":"abc"}"#), None);
    }

    #[test]
    fn test_check_config_valid_with_valid_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_dir = tmp.path().join(".git-ai");
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("config.json");
        fs::write(&config_path, r#"{"some_key": "some_value"}"#).unwrap();

        unsafe { env::set_var("HOME", tmp.path()) };
        let result = check_config_valid();
        assert!(result);
    }

    #[test]
    fn test_check_config_valid_with_invalid_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_dir = tmp.path().join(".git-ai");
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("config.json");
        fs::write(&config_path, "not valid json {{{").unwrap();

        unsafe { env::set_var("HOME", tmp.path()) };
        let result = check_config_valid();
        assert!(!result);
    }

    #[test]
    fn test_check_config_missing_is_ok() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe { env::set_var("HOME", tmp.path()) };
        let result = check_config_valid();
        assert!(result);
    }

    #[test]
    fn test_check_binary_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe { env::set_var("HOME", tmp.path()) };
        let result = check_binary_installed();
        assert!(!result);
    }

    #[test]
    fn test_check_binary_exists() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin_dir = tmp.path().join(".git-ai/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let bin_path = bin_dir.join("git-ai");
        fs::write(&bin_path, "#!/bin/sh\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        unsafe { env::set_var("HOME", tmp.path()) };
        let result = check_binary_installed();
        assert!(result);
    }

    #[test]
    fn test_check_daemon_not_running() {
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe { env::set_var("HOME", tmp.path()) };
        let result = check_daemon_running();
        assert!(!result);
    }
}
