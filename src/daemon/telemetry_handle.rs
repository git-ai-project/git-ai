//! Global daemon telemetry handle for sending events over the control socket.
//!
//! When async_mode is enabled, this handle is initialized once on process start
//! and used by the observability and metrics modules to route events through the
//! daemon instead of writing to per-PID log files.

use crate::daemon::control_api::{CasSyncPayload, ControlRequest, TelemetryEnvelope};
#[cfg(not(any(test, feature = "test-support")))]
use crate::daemon::open_local_socket_stream_with_timeout;
use crate::daemon::send_control_request_with_timeout;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Maximum time to wait for the daemon socket on process start.
#[cfg(not(any(test, feature = "test-support")))]
const DAEMON_TELEMETRY_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Global handle to the daemon control socket for telemetry submission.
static DAEMON_TELEMETRY_HANDLE: OnceLock<Mutex<Option<DaemonTelemetryHandle>>> = OnceLock::new();

struct DaemonTelemetryHandle {
    socket_path: PathBuf,
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
/// with a 2-second timeout.
///
/// Returns the result indicating success, failure, or skip.
pub fn init_daemon_telemetry_handle() -> DaemonTelemetryInitResult {
    // Don't initialize if we're inside the daemon process itself
    if crate::daemon::daemon_process_active() {
        let _ = DAEMON_TELEMETRY_HANDLE.get_or_init(|| Mutex::new(None));
        return DaemonTelemetryInitResult::Skipped;
    }

    // Skip in test builds
    #[cfg(any(test, feature = "test-support"))]
    {
        let _ = DAEMON_TELEMETRY_HANDLE.get_or_init(|| Mutex::new(None));
        return DaemonTelemetryInitResult::Skipped;
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

        // Verify we can actually connect to the control socket
        match open_local_socket_stream_with_timeout(
            &config.control_socket_path,
            DAEMON_TELEMETRY_CONNECT_TIMEOUT,
        ) {
            Ok(_stream) => {
                let handle = DaemonTelemetryHandle {
                    socket_path: config.control_socket_path,
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

/// Submit telemetry envelopes to the daemon over the control socket.
///
/// Fire-and-forget: sends the request and reads the response but doesn't
/// propagate errors (silently drops on failure since telemetry is best-effort).
pub fn submit_telemetry(envelopes: Vec<TelemetryEnvelope>) {
    if envelopes.is_empty() {
        return;
    }

    let Some(handle_mutex) = DAEMON_TELEMETRY_HANDLE.get() else {
        return;
    };
    let Ok(guard) = handle_mutex.lock() else {
        return;
    };
    let Some(handle) = guard.as_ref() else {
        return;
    };

    let request = ControlRequest::SubmitTelemetry { envelopes };
    // Use a short timeout for telemetry — it's best-effort
    let _ =
        send_control_request_with_timeout(&handle.socket_path, &request, Duration::from_secs(1));
}

/// Submit CAS sync records to the daemon over the control socket.
///
/// Fire-and-forget: same as submit_telemetry.
pub fn submit_cas(records: Vec<CasSyncPayload>) {
    if records.is_empty() {
        return;
    }

    let Some(handle_mutex) = DAEMON_TELEMETRY_HANDLE.get() else {
        return;
    };
    let Ok(guard) = handle_mutex.lock() else {
        return;
    };
    let Some(handle) = guard.as_ref() else {
        return;
    };

    let request = ControlRequest::SubmitCas { records };
    let _ =
        send_control_request_with_timeout(&handle.socket_path, &request, Duration::from_secs(1));
}
