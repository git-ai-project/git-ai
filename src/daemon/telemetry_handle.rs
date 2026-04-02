//! Global daemon telemetry handle for sending events over the control socket.
//!
//! When async_mode is enabled, this handle is initialized once on process start
//! and used by the observability and metrics modules to route events through the
//! daemon instead of writing to per-PID log files.
//!
//! The handle maintains a persistent socket connection that is shared across all
//! callers (telemetry, CAS, and potentially checkpoints). This avoids the
//! overhead of opening a new connection for every fire-and-forget event.

use crate::daemon::control_api::{
    CasSyncPayload, ControlRequest, ControlResponse, TelemetryEnvelope,
};
use crate::daemon::domain::RepoContext;
use crate::daemon::{DaemonClientStream, open_local_socket_stream_with_timeout};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Read/write timeout for the persistent daemon socket.
/// Prevents indefinite blocking if the daemon becomes unresponsive.
const DAEMON_SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum time to wait for the daemon socket on process start.
const DAEMON_TELEMETRY_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Global handle to the daemon control socket for telemetry submission.
static DAEMON_TELEMETRY_HANDLE: OnceLock<Mutex<Option<DaemonTelemetryHandle>>> = OnceLock::new();

struct DaemonTelemetryHandle {
    socket_path: PathBuf,
    conn: BufReader<DaemonClientStream>,
}

impl DaemonTelemetryHandle {
    /// Apply read/write timeouts to the underlying socket so that I/O never
    /// blocks indefinitely (which would hold the global mutex and stall the
    /// entire process).
    fn apply_socket_timeouts(stream: &mut DaemonClientStream, socket_path: &std::path::Path) {
        let _ = crate::daemon::set_daemon_client_stream_timeouts(
            stream,
            socket_path,
            DAEMON_SOCKET_IO_TIMEOUT,
        );
    }

    /// Send a control request over the persistent connection and read the response.
    /// On I/O error, attempts to reconnect once before giving up.
    fn send(&mut self, request: &ControlRequest) -> Result<ControlResponse, String> {
        match self.send_inner(request) {
            Ok(resp) => Ok(resp),
            Err(first_err) => {
                // Connection may have been dropped by the daemon; try reconnecting once.
                match open_local_socket_stream_with_timeout(
                    &self.socket_path,
                    Duration::from_secs(1),
                ) {
                    Ok(mut stream) => {
                        Self::apply_socket_timeouts(&mut stream, &self.socket_path);
                        self.conn = BufReader::new(stream);
                        self.send_inner(request)
                            .map_err(|e| format!("reconnect ok but send failed: {}", e))
                    }
                    Err(reconnect_err) => Err(format!(
                        "send failed ({}), reconnect also failed ({})",
                        first_err, reconnect_err
                    )),
                }
            }
        }
    }

    fn send_inner(&mut self, request: &ControlRequest) -> Result<ControlResponse, String> {
        let mut body = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        body.push(b'\n');
        self.conn
            .get_mut()
            .write_all(&body)
            .map_err(|e| format!("write: {}", e))?;
        self.conn
            .get_mut()
            .flush()
            .map_err(|e| format!("flush: {}", e))?;

        let mut line = String::new();
        self.conn
            .read_line(&mut line)
            .map_err(|e| format!("read: {}", e))?;
        if line.trim().is_empty() {
            return Err("empty response from daemon".to_string());
        }
        serde_json::from_str(line.trim()).map_err(|e| format!("parse: {}", e))
    }
}

fn daemon_control_socket_override() -> Option<PathBuf> {
    std::env::var("GIT_AI_DAEMON_CONTROL_SOCKET")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

fn daemon_telemetry_test_harness_active() -> bool {
    std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
        || std::env::var_os("GITAI_TEST_DB_PATH").is_some()
}

fn store_daemon_telemetry_handle(handle: Option<DaemonTelemetryHandle>) {
    let handle_mutex = DAEMON_TELEMETRY_HANDLE.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = handle_mutex.lock() {
        *guard = handle;
    }
}

fn connect_daemon_telemetry_handle(
    socket_path: PathBuf,
    timeout: Duration,
) -> Result<DaemonTelemetryHandle, String> {
    let mut stream =
        open_local_socket_stream_with_timeout(&socket_path, timeout).map_err(|e| e.to_string())?;
    DaemonTelemetryHandle::apply_socket_timeouts(&mut stream, &socket_path);
    Ok(DaemonTelemetryHandle {
        socket_path,
        conn: BufReader::new(stream),
    })
}

/// Result of attempting to initialize the global daemon telemetry handle.
pub enum DaemonTelemetryInitResult {
    /// Successfully connected to daemon.
    Connected,
    /// Failed to connect; contains the error message.
    Failed(String),
    /// Not in daemon mode or already inside the daemon process.
    Skipped,
}

/// Initialize the global daemon telemetry handle.
///
/// Should be called once on process start when `async_mode` is enabled.
/// Attempts to connect to the daemon control socket (starting the daemon if needed)
/// with a 2-second timeout. The connection is kept open and reused for all
/// subsequent telemetry and CAS submissions.
///
/// Returns the result indicating success, failure, or skip.
pub fn init_daemon_telemetry_handle() -> DaemonTelemetryInitResult {
    // Don't initialize if we're inside the daemon process itself
    if crate::daemon::daemon_process_active() {
        store_daemon_telemetry_handle(None);
        return DaemonTelemetryInitResult::Skipped;
    }

    // Integration harnesses set a dedicated test DB path and only inject daemon
    // sockets when they explicitly want authoritative wrapper state. When that
    // marker is absent, a locally run debug/test-support binary should behave
    // like a normal product build and auto-connect to the daemon.
    if let Some(path) = daemon_control_socket_override() {
        match connect_daemon_telemetry_handle(path, DAEMON_TELEMETRY_CONNECT_TIMEOUT) {
            Ok(handle) => {
                store_daemon_telemetry_handle(Some(handle));
                return DaemonTelemetryInitResult::Connected;
            }
            Err(e) => {
                store_daemon_telemetry_handle(None);
                return DaemonTelemetryInitResult::Failed(e.to_string());
            }
        }
    }

    if daemon_telemetry_test_harness_active() {
        store_daemon_telemetry_handle(None);
        return DaemonTelemetryInitResult::Skipped;
    }

    // Try to ensure daemon is running and connect.
    let config =
        match crate::commands::daemon::ensure_daemon_running(DAEMON_TELEMETRY_CONNECT_TIMEOUT) {
            Ok(config) => config,
            Err(e) => {
                store_daemon_telemetry_handle(None);
                return DaemonTelemetryInitResult::Failed(e);
            }
        };

    match connect_daemon_telemetry_handle(
        config.control_socket_path,
        DAEMON_TELEMETRY_CONNECT_TIMEOUT,
    ) {
        Ok(handle) => {
            store_daemon_telemetry_handle(Some(handle));
            DaemonTelemetryInitResult::Connected
        }
        Err(e) => {
            store_daemon_telemetry_handle(None);
            DaemonTelemetryInitResult::Failed(e.to_string())
        }
    }
}

/// Check if the daemon telemetry handle is available for sending events.
pub fn daemon_telemetry_available() -> bool {
    DAEMON_TELEMETRY_HANDLE
        .get()
        .and_then(|m| m.lock().ok())
        .is_some_and(|guard| guard.is_some())
}

/// Send a control request over the shared persistent connection.
///
/// This is the unified entry point used by telemetry, CAS submissions,
/// and any other code that needs to talk to the daemon. The connection
/// is reused across calls; if the socket is dead it will reconnect once.
///
/// Returns the daemon's response, or an error string on failure.
pub fn send_via_daemon(request: &ControlRequest) -> Result<ControlResponse, String> {
    let Some(handle_mutex) = DAEMON_TELEMETRY_HANDLE.get() else {
        return Err("daemon telemetry handle not initialized".to_string());
    };
    let Ok(mut guard) = handle_mutex.lock() else {
        return Err("daemon telemetry handle lock poisoned".to_string());
    };
    let Some(handle) = guard.as_mut() else {
        return Err("daemon telemetry handle not connected".to_string());
    };
    handle.send(request)
}

/// Submit telemetry envelopes to the daemon over the control socket.
///
/// Fire-and-forget: sends the request but doesn't propagate errors
/// (silently drops on failure since telemetry is best-effort).
pub fn submit_telemetry(envelopes: Vec<TelemetryEnvelope>) {
    if envelopes.is_empty() {
        return;
    }
    let request = ControlRequest::SubmitTelemetry { envelopes };
    let _ = send_via_daemon(&request);
}

/// Submit CAS sync records to the daemon over the control socket.
///
/// Fire-and-forget: same as submit_telemetry.
pub fn submit_cas(records: Vec<CasSyncPayload>) {
    if records.is_empty() {
        return;
    }
    let request = ControlRequest::SubmitCas { records };
    let _ = send_via_daemon(&request);
}

/// Send wrapper pre-command state to the daemon.
/// Returns an error if the send fails (caller decides whether to log/ignore).
pub fn send_wrapper_pre_state(
    invocation_id: &str,
    repo_working_dir: &str,
    repo_context: RepoContext,
) -> Result<(), String> {
    let request = ControlRequest::WrapperPreState {
        invocation_id: invocation_id.to_string(),
        repo_working_dir: repo_working_dir.to_string(),
        repo_context,
    };
    send_via_daemon(&request).map(|_| ())
}

/// Send wrapper post-command state to the daemon.
/// Returns an error if the send fails (caller decides whether to log/ignore).
pub fn send_wrapper_post_state(
    invocation_id: &str,
    repo_working_dir: &str,
    repo_context: RepoContext,
) -> Result<(), String> {
    let request = ControlRequest::WrapperPostState {
        invocation_id: invocation_id.to_string(),
        repo_working_dir: repo_working_dir.to_string(),
        repo_context,
    };
    send_via_daemon(&request).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    struct EnvVarGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }

        fn unset(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.original.as_ref() {
                Some(value) => unsafe {
                    std::env::set_var(self.key, value);
                },
                None => unsafe {
                    std::env::remove_var(self.key);
                },
            }
        }
    }

    #[test]
    #[serial]
    fn test_harness_detection_uses_test_db_override_vars() {
        let _unset_test = EnvVarGuard::unset("GIT_AI_TEST_DB_PATH");
        let _unset_legacy_test = EnvVarGuard::unset("GITAI_TEST_DB_PATH");

        assert!(!daemon_telemetry_test_harness_active());

        let _test = EnvVarGuard::set("GIT_AI_TEST_DB_PATH", "/tmp/git-ai-test.db");
        assert!(daemon_telemetry_test_harness_active());
    }

    #[test]
    #[serial]
    fn explicit_control_socket_override_wins_over_test_harness_marker() {
        let _test = EnvVarGuard::set("GIT_AI_TEST_DB_PATH", "/tmp/git-ai-test.db");
        let temp_dir = tempfile::tempdir().expect("tempdir should succeed");
        let socket_path = temp_dir.path().join("control.sock");
        std::fs::write(&socket_path, []).expect("test socket placeholder should be created");
        let _socket = EnvVarGuard::set(
            "GIT_AI_DAEMON_CONTROL_SOCKET",
            socket_path
                .to_str()
                .expect("temp socket path should be utf-8"),
        );

        let override_path = daemon_control_socket_override();
        assert_eq!(override_path.as_deref(), Some(socket_path.as_path()));
        assert!(daemon_telemetry_test_harness_active());
    }
}
