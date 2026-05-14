use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::checkpoint_worker;
use super::protocol::{ControlRequest, ControlResponse};
use super::stats;

/// Per-repository lock registry.
///
/// Ensures that checkpoint requests for the same repository are serialized,
/// preventing concurrent writes from corrupting working log data.
pub struct RepoLocks {
    locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl RepoLocks {
    pub fn new() -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// Get (or create) a lock for the given repository path.
    /// The returned `Arc<Mutex<()>>` can be locked to serialize access.
    pub fn get_lock(&self, repo_path: &Path) -> Arc<Mutex<()>> {
        let mut map = self.locks.lock().unwrap();
        map.entry(repo_path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

pub struct ControlSocket {
    listener: UnixListener,
    shutdown: Arc<AtomicBool>,
    repo_locks: Arc<RepoLocks>,
}

impl ControlSocket {
    pub fn bind(socket_path: &Path, shutdown: Arc<AtomicBool>) -> std::io::Result<Self> {
        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }

        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(socket_path)?;
        listener.set_nonblocking(true)?;

        Ok(Self {
            listener,
            shutdown,
            repo_locks: Arc::new(RepoLocks::new()),
        })
    }

    pub fn run(&self) {
        let poll_interval = Duration::from_millis(100);

        while !self.shutdown.load(Ordering::Relaxed) {
            match self.listener.accept() {
                Ok((stream, _addr)) => {
                    let shutdown = Arc::clone(&self.shutdown);
                    let repo_locks = Arc::clone(&self.repo_locks);
                    thread::spawn(move || {
                        handle_connection(stream, shutdown, repo_locks);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(poll_interval);
                }
                Err(e) => {
                    eprintln!("[git-ai daemon] control accept error: {}", e);
                    thread::sleep(poll_interval);
                }
            }
        }
    }
}

fn handle_connection(
    stream: std::os::unix::net::UnixStream,
    shutdown: Arc<AtomicBool>,
    repo_locks: Arc<RepoLocks>,
) {
    let _ = stream.set_nonblocking(false);
    if let Err(e) = stream.set_read_timeout(Some(Duration::from_secs(30))) {
        eprintln!("[git-ai daemon] control: failed to set read timeout: {}", e);
        return;
    }

    let reader = std::io::BufReader::new(&stream);
    let mut writer = std::io::BufWriter::new(&stream);

    for line_result in reader.lines() {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        match line_result {
            Ok(line) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                let response = handle_request(&line, &shutdown, &repo_locks);
                let is_shutdown = matches!(
                    serde_json::from_str::<ControlRequest>(&line),
                    Ok(ControlRequest::Shutdown)
                );

                let response_json = serde_json::to_string(&response)
                    .unwrap_or_else(|_| r#"{"ok":false}"#.to_string());
                let _ = writeln!(writer, "{}", response_json);
                let _ = writer.flush();

                if is_shutdown {
                    break;
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(_) => {
                break;
            }
        }
    }

    let _ = stream.shutdown(Shutdown::Both);
}

fn handle_request(
    line: &str,
    shutdown: &Arc<AtomicBool>,
    repo_locks: &Arc<RepoLocks>,
) -> ControlResponse {
    let request: ControlRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return ControlResponse::err(format!("invalid request JSON: {}", e));
        }
    };

    match request {
        ControlRequest::Ping => ControlResponse::ok_pong(),
        ControlRequest::Shutdown => {
            shutdown.store(true, Ordering::Relaxed);
            ControlResponse::ok_shutdown()
        }
        ControlRequest::Stats => ControlResponse::ok_stats(stats::format_status_report()),
        ControlRequest::Checkpoint(req) => {
            // Acquire per-repository lock to serialize checkpoint writes
            let repo_path = PathBuf::from(&req.repo_dir);
            let repo_lock = repo_locks.get_lock(&repo_path);
            let _guard = repo_lock.lock().unwrap();

            match checkpoint_worker::process_checkpoint(&req) {
                Ok(count) => {
                    stats::get()
                        .checkpoints_ingested
                        .fetch_add(count as u64, Ordering::Relaxed);
                    ControlResponse::ok_processed(count)
                }
                Err(e) => ControlResponse::err(e),
            }
        }
        ControlRequest::Status(req) => match checkpoint_worker::get_status(&req) {
            Ok(status) => ControlResponse::ok_status(status),
            Err(e) => ControlResponse::err(e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;

    #[test]
    fn ping_pong() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("control.sock");
        let shutdown = Arc::new(AtomicBool::new(false));

        let ctrl = ControlSocket::bind(&socket_path, Arc::clone(&shutdown)).expect("bind failed");

        let shutdown_clone = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            ctrl.run();
        });

        thread::sleep(Duration::from_millis(100));

        let mut client = UnixStream::connect(&socket_path).expect("connect failed");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(client, r#"{{"type":"ping"}}"#).unwrap();
        client.flush().unwrap();

        let mut response = String::new();
        let mut buf = [0u8; 4096];
        let n = client.read(&mut buf).unwrap();
        response.push_str(&String::from_utf8_lossy(&buf[..n]));

        let resp: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
        assert_eq!(resp["ok"], true);
        assert!(resp["version"].is_string());
        assert!(resp["pid"].is_number());

        shutdown_clone.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    #[test]
    fn shutdown_via_control() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("control.sock");
        let shutdown = Arc::new(AtomicBool::new(false));

        let ctrl = ControlSocket::bind(&socket_path, Arc::clone(&shutdown)).expect("bind failed");

        let shutdown_clone = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            ctrl.run();
        });

        thread::sleep(Duration::from_millis(100));

        let mut client = UnixStream::connect(&socket_path).expect("connect failed");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(client, r#"{{"type":"shutdown"}}"#).unwrap();
        client.flush().unwrap();

        // Read response
        let mut buf = [0u8; 4096];
        let _ = client.read(&mut buf);

        // The daemon should have set shutdown flag
        thread::sleep(Duration::from_millis(200));
        assert!(shutdown_clone.load(Ordering::Relaxed));

        handle.join().unwrap();
    }

    #[test]
    fn checkpoint_via_control() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("control.sock");
        let shutdown = Arc::new(AtomicBool::new(false));

        let ctrl = ControlSocket::bind(&socket_path, Arc::clone(&shutdown)).expect("bind failed");

        let shutdown_clone = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            ctrl.run();
        });

        thread::sleep(Duration::from_millis(100));

        // Create a test git repo
        let repo_dir = tempfile::tempdir().unwrap();
        let repo_path = repo_dir.path();
        let git = "git";

        std::process::Command::new(git)
            .args(["init", repo_path.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        std::process::Command::new(git)
            .current_dir(repo_path)
            .args(["config", "user.name", "Test"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        std::process::Command::new(git)
            .current_dir(repo_path)
            .args(["config", "user.email", "test@example.com"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        // Create an initial commit
        std::fs::write(repo_path.join("init.txt"), "init\n").unwrap();
        std::process::Command::new(git)
            .current_dir(repo_path)
            .args(["add", "-A"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        std::process::Command::new(git)
            .current_dir(repo_path)
            .args(["commit", "-m", "initial"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        // Write a file to checkpoint
        std::fs::write(repo_path.join("hello.txt"), "Hello from AI\n").unwrap();

        // Send checkpoint request
        let mut client = UnixStream::connect(&socket_path).expect("connect failed");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let request = serde_json::json!({
            "type": "checkpoint",
            "repo_dir": repo_path.to_str().unwrap(),
            "kind": "ai",
            "files": [{"path": "hello.txt"}],
            "agent": {"tool": "test-agent", "id": "session-1", "model": "test-model"}
        });
        writeln!(client, "{}", request).unwrap();
        client.flush().unwrap();

        let mut buf = [0u8; 4096];
        let n = client.read(&mut buf).unwrap();
        let response: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&buf[..n]).trim()).unwrap();

        assert_eq!(response["ok"], true);
        assert_eq!(response["processed"], 1);

        // Verify checkpoint was written by querying status
        drop(client);
        let mut client2 = UnixStream::connect(&socket_path).expect("connect failed");
        client2
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let status_req = serde_json::json!({
            "type": "status",
            "repo_dir": repo_path.to_str().unwrap()
        });
        writeln!(client2, "{}", status_req).unwrap();
        client2.flush().unwrap();

        let n = client2.read(&mut buf).unwrap();
        let status_resp: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&buf[..n]).trim()).unwrap();

        assert_eq!(status_resp["ok"], true);
        assert_eq!(status_resp["status"]["checkpoint_count"], 1);
        assert_eq!(status_resp["status"]["files"][0], "hello.txt");

        shutdown_clone.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    #[test]
    fn repo_locks_serializes_same_repo() {
        let locks = RepoLocks::new();
        let path = PathBuf::from("/tmp/test-repo");

        // Get lock for same path twice — should be the same Arc
        let lock1 = locks.get_lock(&path);
        let lock2 = locks.get_lock(&path);

        // They should point to the same underlying mutex (same Arc)
        assert!(Arc::ptr_eq(&lock1, &lock2));
    }

    #[test]
    fn repo_locks_different_repos_independent() {
        let locks = RepoLocks::new();
        let path_a = PathBuf::from("/tmp/repo-a");
        let path_b = PathBuf::from("/tmp/repo-b");

        let lock_a = locks.get_lock(&path_a);
        let lock_b = locks.get_lock(&path_b);

        // Different repos get different locks
        assert!(!Arc::ptr_eq(&lock_a, &lock_b));

        // Can acquire both simultaneously without deadlock
        let _guard_a = lock_a.lock().unwrap();
        let _guard_b = lock_b.lock().unwrap();
    }

    #[test]
    fn repo_locks_concurrent_access_serialized() {
        use std::sync::atomic::AtomicU32;

        let locks = Arc::new(RepoLocks::new());
        let counter = Arc::new(AtomicU32::new(0));
        let max_concurrent = Arc::new(AtomicU32::new(0));
        let path = PathBuf::from("/tmp/test-concurrent-repo");

        let num_threads = 8;
        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let locks = Arc::clone(&locks);
                let counter = Arc::clone(&counter);
                let max_concurrent = Arc::clone(&max_concurrent);
                let path = path.clone();
                thread::spawn(move || {
                    let repo_lock = locks.get_lock(&path);
                    let _guard = repo_lock.lock().unwrap();

                    // Increment active count
                    let active = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    // Track maximum concurrency observed
                    max_concurrent.fetch_max(active, Ordering::SeqCst);

                    // Simulate some work
                    thread::sleep(Duration::from_millis(5));

                    // Decrement active count
                    counter.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        // The lock should have serialized access: max concurrent should be 1
        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "repo lock should serialize access - max concurrent should be 1"
        );
    }
}
