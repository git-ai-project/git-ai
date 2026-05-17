use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use super::event_loop;
use super::lifecycle::{
    self, DaemonPaths, Error, acquire_lock, install_signal_handlers, read_pid_file,
    redirect_stderr_to_log, write_pid_file,
};
use super::startup;
use super::stats;
use super::stats_persistence;
use super::telemetry_worker;
use super::trace2_events::Trace2Event;

#[cfg(unix)]
use super::lifecycle::daemonize;
#[cfg(windows)]
use super::lifecycle::daemonize_windows;

static SHUTDOWN: OnceLock<Arc<AtomicBool>> = OnceLock::new();

pub fn shutdown_flag() -> &'static Arc<AtomicBool> {
    SHUTDOWN.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

pub fn run_daemon(foreground: bool) -> Result<(), Error> {
    let paths = DaemonPaths::resolve();
    paths.ensure_dirs()?;

    // Run startup recovery before daemonizing so errors are visible on stderr
    startup::run_startup_recovery(&paths)?;

    if !foreground {
        #[cfg(unix)]
        daemonize()?;
        #[cfg(windows)]
        daemonize_windows()?;
        #[cfg(not(any(unix, windows)))]
        return Err(Error::Generic(
            "daemonize is only supported on unix and windows".to_string(),
        ));
    }

    let _lock = acquire_lock(&paths.lock_file)?;

    write_pid_file(&paths.pid_file)?;

    let shutdown = shutdown_flag().clone();
    install_signal_handlers(shutdown.clone());

    redirect_stderr_to_log(&paths.log_file)?;

    // Disable trace2 for self BEFORE spawning any threads to avoid data races
    disable_trace2_for_self();

    eprintln!("[git-ai] daemon started (pid {})", std::process::id());

    // Record start time in persisted stats
    let start_timestamp = chrono_iso_now();
    stats_persistence::update_last_started(&start_timestamp);

    // Create channel for trace2 events
    let (event_tx, event_rx) = mpsc::channel::<Trace2Event>();

    // Start the trace2 socket listener thread
    let listener_handle = start_trace2_listener(&paths, event_tx, shutdown.clone())?;

    // Start the control socket listener thread
    let control_handle = start_control_socket(&paths, shutdown.clone())?;

    // Start the telemetry worker (3-second flush loop)
    let telemetry_handle = telemetry_worker::spawn_telemetry_worker(shutdown.clone());

    // Run the event loop on the main thread (blocks until shutdown)
    event_loop::run_event_loop(event_rx, shutdown.clone(), telemetry_handle);

    // Wait for listener threads to finish
    if let Some(handle) = listener_handle {
        let _ = handle.join();
    }
    if let Some(handle) = control_handle {
        let _ = handle.join();
    }

    // Save stats before final shutdown
    eprintln!("[git-ai] saving stats before shutdown...");
    stats_persistence::save_stats(stats::get());

    eprintln!("[git-ai] daemon shutting down");
    let _ = fs::remove_file(&paths.pid_file);

    Ok(())
}

/// Start the trace2 listener on a background thread.
/// Returns the thread handle (None on platforms where the listener isn't supported yet).
fn start_trace2_listener(
    paths: &DaemonPaths,
    event_tx: mpsc::Sender<Trace2Event>,
    shutdown: Arc<AtomicBool>,
) -> Result<Option<thread::JoinHandle<()>>, Error> {
    #[cfg(unix)]
    {
        use super::trace2_listener::Trace2Listener;

        let listener = Trace2Listener::bind(&paths.trace2_sock, shutdown.clone())
            .map_err(|e| Error::Generic(format!("failed to bind trace2 socket: {}", e)))?;

        eprintln!(
            "[git-ai] trace2 listener bound to {}",
            paths.trace2_sock.display()
        );

        let handle = thread::Builder::new()
            .name("trace2-listener".to_string())
            .spawn(move || {
                listener.run(event_tx);
            })
            .map_err(|e| Error::Generic(format!("failed to spawn listener thread: {}", e)))?;

        Ok(Some(handle))
    }

    #[cfg(windows)]
    {
        use super::trace2_listener_win::Trace2ListenerWin;

        let listener = Trace2ListenerWin::bind(&paths.trace2_sock, shutdown.clone())
            .map_err(|e| Error::Generic(format!("failed to bind trace2 named pipe: {}", e)))?;

        eprintln!(
            "[git-ai] trace2 listener bound to {}",
            paths.trace2_sock.display()
        );

        let handle = thread::Builder::new()
            .name("trace2-listener".to_string())
            .spawn(move || {
                listener.run(event_tx);
            })
            .map_err(|e| Error::Generic(format!("failed to spawn listener thread: {}", e)))?;

        Ok(Some(handle))
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (paths, event_tx, shutdown);
        eprintln!("[git-ai] trace2 listener not yet supported on this platform");
        Ok(None)
    }
}

/// Start the control socket on a background thread.
fn start_control_socket(
    paths: &DaemonPaths,
    shutdown: Arc<AtomicBool>,
) -> Result<Option<thread::JoinHandle<()>>, Error> {
    #[cfg(unix)]
    {
        use super::control_socket::ControlSocket;

        let ctrl = ControlSocket::bind(&paths.control_sock, shutdown)
            .map_err(|e| Error::Generic(format!("failed to bind control socket: {}", e)))?;

        eprintln!(
            "[git-ai] control socket bound to {}",
            paths.control_sock.display()
        );

        let handle = thread::Builder::new()
            .name("control-socket".to_string())
            .spawn(move || {
                ctrl.run();
            })
            .map_err(|e| Error::Generic(format!("failed to spawn control thread: {}", e)))?;

        Ok(Some(handle))
    }

    #[cfg(not(unix))]
    {
        let _ = (paths, shutdown);
        eprintln!(
            "[git-ai] control socket not available on Windows; daemon management via CLI is limited"
        );
        Ok(None)
    }
}

fn disable_trace2_for_self() {
    unsafe {
        std::env::set_var("GIT_TRACE2_EVENT", "0");
    }
}

/// Simple ISO 8601 timestamp for stats persistence.
fn chrono_iso_now() -> String {
    use std::time::SystemTime;
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let mins = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;

    // Simple days-to-YMD conversion (same algorithm as lifecycle.rs)
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hours, mins, s
    )
}

pub fn stop_daemon() -> Result<(), Error> {
    let paths = DaemonPaths::resolve();

    let daemon_pid = match read_pid_file(&paths.pid_file) {
        Some(p) => p,
        None => {
            eprintln!("[git-ai] daemon is not running");
            return Ok(());
        }
    };

    if !lifecycle::is_process_alive(daemon_pid.pid) {
        let _ = fs::remove_file(&paths.pid_file);
        eprintln!("[git-ai] daemon is not running (stale pid file removed)");
        return Ok(());
    }

    #[cfg(unix)]
    {
        // Try graceful shutdown via control socket first
        let shutdown_sent =
            super::control_client::send_request(&paths.control_sock, r#"{"type":"shutdown"}"#)
                .is_ok();

        if !shutdown_sent {
            // Fall back to SIGTERM
            unsafe {
                libc::kill(daemon_pid.pid as i32, libc::SIGTERM);
            }
        }

        // Wait up to 5 seconds for graceful exit
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(100));
            if !lifecycle::is_process_alive(daemon_pid.pid) {
                eprintln!("[git-ai] daemon stopped");
                return Ok(());
            }
        }

        // SIGKILL as last resort
        unsafe {
            libc::kill(daemon_pid.pid as i32, libc::SIGKILL);
        }
        thread::sleep(Duration::from_millis(100));

        if !lifecycle::is_process_alive(daemon_pid.pid) {
            eprintln!("[git-ai] daemon killed (did not exit gracefully within 5s)");
        } else {
            eprintln!(
                "[git-ai] daemon (pid {}) did not exit even after SIGKILL",
                daemon_pid.pid
            );
        }
    }

    #[cfg(not(unix))]
    {
        eprintln!("[git-ai] stop is only supported on unix");
    }

    Ok(())
}

/// Restart the daemon: gracefully stop the current instance (saving stats),
/// then start a new one.
pub fn restart_daemon() -> Result<(), Error> {
    let paths = DaemonPaths::resolve();

    // Save stats from the running daemon before stopping
    if let Some(daemon_pid) = read_pid_file(&paths.pid_file)
        && lifecycle::is_process_alive(daemon_pid.pid)
    {
        eprintln!("[git-ai] saving stats before restart...");
        // Query live stats and persist them via the control socket
        #[cfg(unix)]
        {
            // The stats will be saved by the daemon during its shutdown sequence.
            // We just need to trigger graceful shutdown.
        }
    }

    // Stop the running daemon
    stop_daemon()?;

    // Small delay to ensure the old process has fully released resources
    thread::sleep(Duration::from_millis(200));

    // Start a new daemon instance
    eprintln!("[git-ai] starting new daemon instance...");
    run_daemon(false)
}

pub fn print_status() {
    let paths = DaemonPaths::resolve();

    match read_pid_file(&paths.pid_file) {
        Some(info) => {
            if lifecycle::is_process_alive(info.pid) {
                println!(
                    "daemon running (pid {}, started {}, version {})",
                    info.pid, info.started_at, info.version
                );
                // Try to get live stats from the control socket
                #[cfg(unix)]
                {
                    if let Some(stats) = query_live_stats(&paths.control_sock) {
                        println!("{}", stats);
                    }
                }
            } else {
                println!("daemon not running (stale pid file, last pid {})", info.pid);
            }
        }
        None => {
            println!("daemon not running");
        }
    }
}

#[cfg(unix)]
fn query_live_stats(control_sock: &std::path::Path) -> Option<String> {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(control_sock).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    writeln!(stream, r#"{{"type":"stats"}}"#).ok()?;
    stream.flush().ok()?;

    let reader = std::io::BufReader::new(&stream);
    let line = reader.lines().next()?.ok()?;
    let resp: serde_json::Value = serde_json::from_str(&line).ok()?;
    resp.get("stats")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}

/// Ensure the daemon is running, spawning it in the background if needed.
/// Blocks until the daemon is ready (PID file with live process), up to a timeout.
pub fn ensure_daemon_running() {
    if std::env::var("GIT_AI_NO_DAEMON").as_deref() == Ok("1") {
        return;
    }

    let paths = DaemonPaths::resolve();

    if is_daemon_alive(&paths) {
        return;
    }

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return,
    };

    // Use "bg run" (foreground mode) since we already handle detachment via
    // spawn() on Unix and creation_flags on Windows.
    #[cfg(unix)]
    {
        use std::process::Command;
        let _ = Command::new(&exe)
            .args(["bg", "run"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Command;
        const CREATE_NO_WINDOW_FLAG: u32 = 0x08000000;
        const DETACHED_PROCESS_FLAG: u32 = 0x00000008;
        let _ = Command::new(&exe)
            .args(["bg", "run"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW_FLAG | DETACHED_PROCESS_FLAG)
            .spawn();
    }

    // Wait for the daemon to be ready (up to 5 seconds)
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if is_daemon_alive(&paths) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn is_daemon_alive(paths: &DaemonPaths) -> bool {
    if let Some(info) = read_pid_file(&paths.pid_file) {
        lifecycle::is_process_alive(info.pid)
    } else {
        false
    }
}
