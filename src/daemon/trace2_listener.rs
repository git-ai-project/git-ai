use std::io::BufRead;
use std::net::Shutdown;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use crate::daemon::stats;
use crate::daemon::trace2_events::{Trace2Event, parse_trace2_line};

const MAX_CONCURRENT_CONNECTIONS: usize = 64;
const MAX_LINE_BYTES: usize = 256 * 1024; // 256 KB max per trace2 event line
static ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

/// Listens on a Unix domain socket for git trace2 events.
///
/// Git processes configured with `GIT_TRACE2_EVENT=af_unix:<socket_path>` will
/// connect to this socket and stream newline-delimited JSON events.
pub struct Trace2Listener {
    listener: UnixListener,
    shutdown: Arc<AtomicBool>,
}

impl Trace2Listener {
    /// Bind the trace2 socket. Removes a stale socket file if it exists.
    /// Socket directory is restricted to owner-only (0700) to prevent
    /// local users from connecting and injecting events.
    pub fn bind(socket_path: &Path, shutdown: Arc<AtomicBool>) -> std::io::Result<Self> {
        // Remove stale socket file if present
        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }

        // Ensure the parent directory exists with restricted permissions
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }

        let listener = UnixListener::bind(socket_path)?;

        // Restrict socket file to owner-only
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
        }

        // Set non-blocking so we can periodically check the shutdown flag
        listener.set_nonblocking(true)?;

        Ok(Self { listener, shutdown })
    }

    /// Run the accept loop. Spawns a thread per connection.
    ///
    /// Each connection reads newline-delimited JSON and sends parsed events to
    /// the provided channel. Returns when the shutdown flag is set.
    pub fn run(&self, event_tx: Sender<Trace2Event>) {
        let poll_interval = Duration::from_millis(100);

        while !self.shutdown.load(Ordering::Relaxed) {
            match self.listener.accept() {
                Ok((stream, _addr)) => {
                    stats::get()
                        .trace2_connections
                        .fetch_add(1, Ordering::Relaxed);

                    let current = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
                    if current >= MAX_CONCURRENT_CONNECTIONS {
                        // At limit: handle inline (blocking accept loop briefly is better than OOM)
                        let tx = event_tx.clone();
                        let shutdown = Arc::clone(&self.shutdown);
                        handle_connection(stream, tx, shutdown);
                    } else {
                        // Below limit: spawn thread
                        ACTIVE_CONNECTIONS.fetch_add(1, Ordering::Relaxed);
                        let tx = event_tx.clone();
                        let shutdown = Arc::clone(&self.shutdown);
                        thread::spawn(move || {
                            handle_connection(stream, tx, shutdown);
                            ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(poll_interval);
                }
                Err(e) => {
                    eprintln!("[git-ai daemon] accept error: {}", e);
                    thread::sleep(poll_interval);
                }
            }
        }
    }
}

/// Handle a single client connection, reading trace2 JSON lines until EOF or shutdown.
fn handle_connection(
    stream: std::os::unix::net::UnixStream,
    event_tx: Sender<Trace2Event>,
    shutdown: Arc<AtomicBool>,
) {
    let _ = stream.set_nonblocking(false);
    if let Err(e) = stream.set_read_timeout(Some(Duration::from_secs(5))) {
        eprintln!("[git-ai daemon] failed to set read timeout: {}", e);
        return;
    }

    let mut reader = std::io::BufReader::new(&stream);
    let mut line_buf = String::new();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => break, // EOF
            Ok(n) if n > MAX_LINE_BYTES => continue, // discard oversized lines
            Ok(_) => {
                if let Some(event) = parse_trace2_line(&line_buf)
                    && event_tx.send(event).is_err()
                {
                    break;
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => break,
        }

        if line_buf.len() > MAX_LINE_BYTES {
            // read_line already read into the buffer; just discard and move on
            continue;
        }
    }

    let _ = stream.shutdown(Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    #[test]
    fn listener_accepts_and_parses_events() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("trace2.sock");
        let shutdown = Arc::new(AtomicBool::new(false));

        let listener =
            Trace2Listener::bind(&socket_path, Arc::clone(&shutdown)).expect("bind failed");

        let (tx, rx) = mpsc::channel();

        let shutdown_clone = Arc::clone(&shutdown);
        let listener_thread = thread::spawn(move || {
            listener.run(tx);
        });

        // Give the listener a moment to start
        thread::sleep(Duration::from_millis(100));

        // Connect as a client and send trace2 events
        let mut client = UnixStream::connect(&socket_path).expect("connect failed");
        let events = concat!(
            r#"{"event":"start","sid":"test-sid-1","thread":"main","time":"2024-01-01T00:00:00Z","argv":["git","commit","-m","hello"]}"#,
            "\n",
            r#"{"event":"def_repo","sid":"test-sid-1","thread":"main","repo":1,"worktree":"/tmp/repo"}"#,
            "\n",
            r#"{"event":"cmd_name","sid":"test-sid-1","thread":"main","name":"commit"}"#,
            "\n",
            r#"{"event":"exit","sid":"test-sid-1","thread":"main","t_abs":0.01,"code":0}"#,
            "\n",
        );
        client.write_all(events.as_bytes()).unwrap();
        client.shutdown(Shutdown::Write).unwrap();

        // Collect events with a timeout
        let mut received = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(event) => received.push(event),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if received.len() >= 4 {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        // Signal shutdown
        shutdown_clone.store(true, Ordering::Relaxed);
        listener_thread.join().unwrap();

        assert_eq!(
            received.len(),
            4,
            "expected 4 events, got {}",
            received.len()
        );

        // Verify event types
        assert!(matches!(received[0], Trace2Event::Start { .. }));
        assert!(matches!(received[1], Trace2Event::DefRepo { .. }));
        assert!(matches!(received[2], Trace2Event::CmdName { .. }));
        assert!(matches!(received[3], Trace2Event::CommandExit { .. }));
    }

    #[test]
    fn listener_removes_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("stale.sock");

        // Create a stale file
        std::fs::write(&socket_path, "stale").unwrap();
        assert!(socket_path.exists());

        let shutdown = Arc::new(AtomicBool::new(true)); // immediate shutdown
        let listener = Trace2Listener::bind(&socket_path, shutdown);
        assert!(listener.is_ok(), "should bind after removing stale socket");
    }

    #[test]
    fn listener_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("subdir").join("nested").join("trace2.sock");

        let shutdown = Arc::new(AtomicBool::new(true));
        let listener = Trace2Listener::bind(&socket_path, shutdown);
        assert!(listener.is_ok(), "should create parent directories");
    }

    #[test]
    fn listener_shutdown_stops_loop() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("shutdown.sock");
        let shutdown = Arc::new(AtomicBool::new(false));

        let listener =
            Trace2Listener::bind(&socket_path, Arc::clone(&shutdown)).expect("bind failed");

        let (tx, _rx) = mpsc::channel();

        let shutdown_clone = Arc::clone(&shutdown);
        let listener_thread = thread::spawn(move || {
            listener.run(tx);
        });

        // Immediately signal shutdown
        thread::sleep(Duration::from_millis(100));
        shutdown_clone.store(true, Ordering::Relaxed);

        // Should exit within a reasonable time
        let result = listener_thread.join();
        assert!(
            result.is_ok(),
            "listener thread should exit cleanly on shutdown"
        );
    }
}
