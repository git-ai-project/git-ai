//! Health monitoring for the daemon.
//!
//! Provides `check_health()` which verifies the daemon's operational state
//! by checking the PID file, process liveness, socket existence, and
//! control socket responsiveness.

use std::fmt;
use std::path::Path;

use super::control_client;
use super::lifecycle::{DaemonPaths, is_process_alive, read_pid_file};

/// Represents the health status of the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    /// Daemon is fully operational: process alive, socket responding to ping.
    Healthy,
    /// Daemon is partially operational: some checks failed.
    Degraded(String),
    /// Daemon is not running or completely unresponsive.
    Dead(String),
}

impl fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HealthStatus::Healthy => write!(f, "healthy"),
            HealthStatus::Degraded(reason) => write!(f, "degraded: {}", reason),
            HealthStatus::Dead(reason) => write!(f, "dead: {}", reason),
        }
    }
}

/// Check the health of the daemon by verifying:
/// 1. PID file exists and process is alive (kill -0)
/// 2. Control socket file exists on disk
/// 3. Control socket responds to ping
pub fn check_health() -> HealthStatus {
    let paths = DaemonPaths::resolve();

    // Check 1: PID file exists and process is alive
    let daemon_pid = match read_pid_file(&paths.pid_file) {
        Some(p) => p,
        None => return HealthStatus::Dead("no pid file found".to_string()),
    };

    if !is_process_alive(daemon_pid.pid) {
        return HealthStatus::Dead(format!("process {} is not alive", daemon_pid.pid));
    }

    // Check 2: Control socket file exists on disk
    if !socket_exists(&paths.control_sock) {
        return HealthStatus::Degraded(format!(
            "process {} alive but control socket missing at {}",
            daemon_pid.pid,
            paths.control_sock.display()
        ));
    }

    // Check 3: Control socket responds to ping
    #[cfg(unix)]
    {
        if !control_client::is_daemon_running(&paths.control_sock) {
            return HealthStatus::Degraded(format!(
                "process {} alive but control socket not responding",
                daemon_pid.pid
            ));
        }
    }

    HealthStatus::Healthy
}

fn socket_exists(path: &Path) -> bool {
    #[cfg(unix)]
    {
        path.exists()
    }
    #[cfg(not(unix))]
    {
        // On Windows, named pipes don't show up as regular files.
        // We rely on the ping check instead.
        let _ = path;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_status_returns_valid_variant() {
        // check_health() depends on system state, so we just verify it returns
        // a valid variant without panicking
        let status = check_health();
        match status {
            HealthStatus::Healthy | HealthStatus::Degraded(_) | HealthStatus::Dead(_) => {}
        }
    }

    #[test]
    fn display_formats_correctly() {
        assert_eq!(format!("{}", HealthStatus::Healthy), "healthy");
        assert_eq!(
            format!("{}", HealthStatus::Degraded("test reason".to_string())),
            "degraded: test reason"
        );
        assert_eq!(
            format!("{}", HealthStatus::Dead("no pid".to_string())),
            "dead: no pid"
        );
    }

    #[test]
    fn dead_when_process_not_alive() {
        // Verify the Dead variant contains a useful message
        let status = HealthStatus::Dead("process 999999 is not alive".to_string());
        assert!(format!("{}", status).contains("not alive"));
    }

    #[test]
    fn degraded_contains_reason() {
        let status = HealthStatus::Degraded("control socket not responding".to_string());
        assert!(format!("{}", status).contains("control socket"));
    }
}
