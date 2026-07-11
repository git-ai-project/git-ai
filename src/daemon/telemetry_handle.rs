//! Global daemon telemetry handle for sending events over the control socket.
//!
//! When daemon mode is active, this handle is initialized once on process start
//! and used by the observability and metrics modules to route events through the
//! daemon instead of writing to per-PID log files.
//!
//! The handle maintains a persistent socket connection that is shared across all
//! callers (telemetry, CAS, and potentially checkpoints). This avoids the
//! overhead of opening a new connection for every fire-and-forget event.

use crate::daemon::control_api::{
    CasSyncPayload, ControlRequest, ControlResponse, TelemetryEnvelope,
};
use crate::daemon::{DaemonClientStream, open_local_socket_stream_with_timeout};
use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Read/write timeout for the persistent daemon socket.
/// Prevents indefinite blocking if the daemon becomes unresponsive.
const DAEMON_SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum time to wait for the daemon socket on process start.
#[cfg(not(any(test, feature = "test-support")))]
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
        Self::apply_socket_timeout(stream, socket_path, DAEMON_SOCKET_IO_TIMEOUT);
    }

    fn apply_socket_timeout(
        stream: &mut DaemonClientStream,
        socket_path: &std::path::Path,
        timeout: Duration,
    ) {
        let _ = crate::daemon::set_daemon_client_stream_timeouts(stream, socket_path, timeout);
    }

    fn reconnect(&mut self) -> Result<(), String> {
        let mut stream =
            open_local_socket_stream_with_timeout(&self.socket_path, Duration::from_secs(1))
                .map_err(|error| error.to_string())?;
        Self::apply_socket_timeouts(&mut stream, &self.socket_path);
        self.conn = BufReader::new(stream);
        Ok(())
    }

    /// Send a control request over the persistent connection and read the response.
    /// On I/O error, attempts to reconnect once before giving up.
    fn send(&mut self, request: &ControlRequest) -> Result<ControlResponse, String> {
        match self.send_inner(request) {
            Ok(resp) => Ok(resp),
            Err(error) if matches!(request, ControlRequest::CheckpointRun { .. }) => {
                // The daemon may have accepted the checkpoint before the response
                // failed, so never replay it. Heal the persistent socket for the
                // next request without changing this request's error result.
                match self.reconnect() {
                    Ok(()) => Err(error),
                    Err(reconnect_error) => Err(format!(
                        "send failed ({error}), reconnect also failed ({reconnect_error})"
                    )),
                }
            }
            Err(first_err) => {
                // Connection may have been dropped by the daemon; try reconnecting once.
                match self.reconnect() {
                    Ok(()) => self
                        .send_inner(request)
                        .map_err(|e| format!("reconnect ok but send failed: {}", e)),
                    Err(reconnect_err) => Err(format!(
                        "send failed ({}), reconnect also failed ({})",
                        first_err, reconnect_err
                    )),
                }
            }
        }
    }

    fn send_inner(&mut self, request: &ControlRequest) -> Result<ControlResponse, String> {
        Self::apply_socket_timeout(
            self.conn.get_mut(),
            &self.socket_path,
            crate::daemon::control_request_response_timeout(request),
        );
        let result = self.exchange(request);
        Self::apply_socket_timeouts(self.conn.get_mut(), &self.socket_path);
        result
    }

    fn exchange(&mut self, request: &ControlRequest) -> Result<ControlResponse, String> {
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

        let line = crate::daemon::read_daemon_client_line(
            &mut self.conn,
            &self.socket_path,
            crate::daemon::control_request_response_timeout(request),
        )
        .map_err(|e| format!("read: {}", e))?;
        if line.trim().is_empty() {
            return Err("empty response from daemon".to_string());
        }
        serde_json::from_str(line.trim()).map_err(|e| format!("parse: {}", e))
    }
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
/// Should be called once on process start when daemon mode is active.
/// Attempts to connect to the daemon control socket (starting the daemon if needed)
/// with a 2-second timeout. The connection is kept open and reused for all
/// subsequent telemetry and CAS submissions.
///
/// Returns the result indicating success, failure, or skip.
pub fn init_daemon_telemetry_handle() -> DaemonTelemetryInitResult {
    // Don't initialize if we're inside the daemon process itself
    if crate::daemon::daemon_process_active() {
        let _ = DAEMON_TELEMETRY_HANDLE.get_or_init(|| Mutex::new(None));
        return DaemonTelemetryInitResult::Skipped;
    }

    // In test builds, only connect if the daemon control socket is explicitly set.
    #[cfg(any(test, feature = "test-support"))]
    {
        let socket_path = std::env::var("GIT_AI_DAEMON_CONTROL_SOCKET")
            .ok()
            .filter(|p| !p.trim().is_empty())
            .map(PathBuf::from)
            .filter(|p| p.exists());

        match socket_path {
            Some(path) => {
                match open_local_socket_stream_with_timeout(&path, Duration::from_secs(2)) {
                    Ok(mut stream) => {
                        DaemonTelemetryHandle::apply_socket_timeouts(&mut stream, &path);
                        let handle = DaemonTelemetryHandle {
                            socket_path: path,
                            conn: BufReader::new(stream),
                        };
                        let _ = DAEMON_TELEMETRY_HANDLE.get_or_init(|| Mutex::new(Some(handle)));
                        DaemonTelemetryInitResult::Connected
                    }
                    Err(e) => {
                        let _ = DAEMON_TELEMETRY_HANDLE.get_or_init(|| Mutex::new(None));
                        DaemonTelemetryInitResult::Failed(e.to_string())
                    }
                }
            }
            None => {
                let _ = DAEMON_TELEMETRY_HANDLE.get_or_init(|| Mutex::new(None));
                DaemonTelemetryInitResult::Skipped
            }
        }
    }

    #[cfg(not(any(test, feature = "test-support")))]
    {
        // Try to ensure daemon is running and connect
        let config = match crate::commands::daemon::ensure_daemon_running(
            DAEMON_TELEMETRY_CONNECT_TIMEOUT,
        ) {
            Ok(config) => config,
            Err(e) => {
                let _ = DAEMON_TELEMETRY_HANDLE.get_or_init(|| Mutex::new(None));
                return DaemonTelemetryInitResult::Failed(e);
            }
        };

        // Open a persistent connection to the control socket
        match open_local_socket_stream_with_timeout(
            &config.control_socket_path,
            DAEMON_TELEMETRY_CONNECT_TIMEOUT,
        ) {
            Ok(mut stream) => {
                DaemonTelemetryHandle::apply_socket_timeouts(
                    &mut stream,
                    &config.control_socket_path,
                );
                let handle = DaemonTelemetryHandle {
                    socket_path: config.control_socket_path,
                    conn: BufReader::new(stream),
                };
                let _ = DAEMON_TELEMETRY_HANDLE.get_or_init(|| Mutex::new(Some(handle)));
                DaemonTelemetryInitResult::Connected
            }
            Err(e) => {
                let _ = DAEMON_TELEMETRY_HANDLE.get_or_init(|| Mutex::new(None));
                DaemonTelemetryInitResult::Failed(e.to_string())
            }
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

/// Signal the daemon that new notes are pending in `notes-db` and should be
/// flushed to the remote backend.
///
/// Fire-and-forget: silently drops on failure (flush will happen on the next
/// periodic tick regardless).
pub fn submit_notes() {
    let request = ControlRequest::FlushNotes;
    let _ = send_via_daemon(&request);
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;
    use crate::authorship::working_log::CheckpointKind;
    use crate::commands::checkpoint_agent::orchestrator::{
        BaseCommit, CheckpointFile, CheckpointRequest,
    };
    use crate::daemon::checkpoint::PreparedPathRole;
    use interprocess::local_socket::{ListenerOptions, prelude::*};
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::thread;

    fn checkpoint_request(trace_id: &str) -> ControlRequest {
        ControlRequest::CheckpointRun {
            request: Box::new(CheckpointRequest {
                trace_id: trace_id.to_string(),
                checkpoint_kind: CheckpointKind::Human,
                agent_id: None,
                files: vec![CheckpointFile {
                    path: PathBuf::from("test.txt"),
                    content: None,
                    repo_work_dir: PathBuf::from("/tmp/repo"),
                    base_commit: BaseCommit::Initial,
                }],
                path_role: PreparedPathRole::WillEdit,
                stream_source: None,
                metadata: Default::default(),
            }),
        }
    }

    fn bind_test_listener(socket_path: &std::path::Path) -> LocalSocketListener {
        ListenerOptions::new()
            .name(crate::daemon::local_socket_name(socket_path).unwrap())
            .create_sync()
            .unwrap()
    }

    fn connect_test_handle(socket_path: &std::path::Path) -> DaemonTelemetryHandle {
        let mut stream =
            open_local_socket_stream_with_timeout(socket_path, Duration::from_secs(1)).unwrap();
        DaemonTelemetryHandle::apply_socket_timeouts(&mut stream, socket_path);
        DaemonTelemetryHandle {
            socket_path: socket_path.to_path_buf(),
            conn: BufReader::new(stream),
        }
    }

    fn read_request_and_respond(stream: LocalSocketStream, delay: Duration) -> ControlRequest {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let request = serde_json::from_str(line.trim()).unwrap();
        thread::sleep(delay);
        let response = serde_json::to_string(&ControlResponse::ok(None, None)).unwrap();
        reader.get_mut().write_all(response.as_bytes()).unwrap();
        reader.get_mut().write_all(b"\n").unwrap();
        reader.get_mut().flush().unwrap();
        request
    }

    #[test]
    fn persistent_handle_honors_long_control_request_timeout() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("control.sock");
        let listener = bind_test_listener(&socket_path);
        let server = thread::spawn(move || {
            let stream = listener.incoming().next().unwrap().unwrap();
            read_request_and_respond(stream, Duration::from_millis(2_100))
        });
        let mut handle = connect_test_handle(&socket_path);

        let response = handle.send(&ControlRequest::SyncFamily {
            repo_working_dir: "/tmp/repo".to_string(),
        });

        assert!(
            response.is_ok(),
            "long control request failed: {response:?}"
        );
        assert!(matches!(
            server.join().unwrap(),
            ControlRequest::SyncFamily { .. }
        ));
    }

    #[test]
    fn failed_checkpoint_heals_connection_without_replaying_request() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("control.sock");
        let listener = bind_test_listener(&socket_path);
        let server = thread::spawn(move || {
            let first_stream = listener.incoming().next().unwrap().unwrap();
            let mut first_reader = BufReader::new(first_stream);
            let mut first_line = String::new();
            first_reader.read_line(&mut first_line).unwrap();
            drop(first_reader);

            let second_stream = listener.incoming().next().unwrap().unwrap();
            let second_request = read_request_and_respond(second_stream, Duration::from_millis(0));
            (first_line, second_request)
        });
        let mut handle = connect_test_handle(&socket_path);

        assert!(handle.send(&checkpoint_request("first")).is_err());
        let second = handle.send(&checkpoint_request("second"));
        if second.is_err() {
            // Unblock the test server on the red path where the failed checkpoint
            // did not establish a fresh connection.
            let _ = open_local_socket_stream_with_timeout(&socket_path, Duration::from_secs(1));
        }

        assert!(
            second.is_ok(),
            "healed checkpoint connection failed: {second:?}"
        );
        let (first_line, second_request) = server.join().unwrap();
        let first_request: ControlRequest = serde_json::from_str(first_line.trim()).unwrap();
        assert!(matches!(
            first_request,
            ControlRequest::CheckpointRun { .. }
        ));
        let ControlRequest::CheckpointRun { request } = second_request else {
            panic!("reconnected socket received a replay or non-checkpoint request");
        };
        assert_eq!(request.trace_id, "second");
    }
}
