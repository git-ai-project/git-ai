//! Startup recovery and cleanup for the daemon.
//!
//! On startup, performs:
//! - Stale socket file cleanup
//! - Stale lock file detection (PID dead → break lock)
//! - Log rotation (rotate if > 10MB, keeping up to 3 rotated files)

use std::fs;
use std::path::Path;

use super::lifecycle::{DaemonPaths, Error, is_process_alive, read_pid_file};
use super::log_rotation;

/// Run all startup recovery checks before the daemon begins its main loop.
/// This should be called early in `run_daemon()`, after paths are resolved
/// but before acquiring the lock.
pub fn run_startup_recovery(paths: &DaemonPaths) -> Result<(), Error> {
    cleanup_stale_pid(paths)?;
    cleanup_stale_sockets(paths);
    log_rotation::rotate_logs_if_needed(&paths.log_file);
    Ok(())
}

/// If a PID file exists but the process is dead, remove it and the lock file
/// so the new daemon instance can start.
fn cleanup_stale_pid(paths: &DaemonPaths) -> Result<(), Error> {
    if let Some(daemon_pid) = read_pid_file(&paths.pid_file) {
        if !is_process_alive(daemon_pid.pid) {
            eprintln!(
                "[git-ai] removing stale pid file (pid {} is dead)",
                daemon_pid.pid
            );
            let _ = fs::remove_file(&paths.pid_file);
            let _ = fs::remove_file(&paths.lock_file);
        } else {
            return Err(Error::AlreadyRunning(daemon_pid.pid));
        }
    }
    Ok(())
}

/// Remove leftover socket files from a previous unclean shutdown.
/// The trace2 listener and control socket both remove stale sockets on bind,
/// but doing it here as well handles the case where bind itself failed previously.
fn cleanup_stale_sockets(paths: &DaemonPaths) {
    remove_socket_if_stale(&paths.trace2_sock);
    remove_socket_if_stale(&paths.control_sock);
}

fn remove_socket_if_stale(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        if path.exists() {
            // Try connecting — if it fails, the socket is stale
            if UnixStream::connect(path).is_err() {
                eprintln!("[git-ai] removing stale socket: {}", path.display());
                let _ = fs::remove_file(path);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_stale_pid_file_when_process_dead() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("daemon.pid.json");

        // Write a PID file with a definitely-dead PID
        let content = r#"{"pid":999999999,"started_at":"2024-01-01T00:00:00Z","version":"0.1.0"}"#;
        fs::write(&pid_file, content).unwrap();

        let paths = DaemonPaths {
            base_dir: dir.path().to_path_buf(),
            lock_file: dir.path().join("daemon.lock"),
            pid_file: pid_file.clone(),
            log_file: dir.path().join("daemon.log"),
            trace2_sock: dir.path().join("trace2.sock"),
            control_sock: dir.path().join("control.sock"),
        };

        let result = run_startup_recovery(&paths);
        assert!(result.is_ok());
        assert!(!pid_file.exists());
    }
}
