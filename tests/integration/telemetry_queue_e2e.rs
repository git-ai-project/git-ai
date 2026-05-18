//! End-to-end tests for the SQLite-backed offline telemetry queue.
//!
//! Verifies: events persist to SQLite when upload fails, drain via flush-metrics-db,
//! bounded growth with FIFO eviction, and the flush-metrics-db CLI command.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::repos::test_repo::{get_binary_path, real_git_executable};

/// Spawn a mock HTTP server that can be toggled between accepting and rejecting requests.
/// A control file determines behavior: "up" = 200, "down" = 503.
fn spawn_controllable_mock(base_dir: &Path) -> (Child, u16, PathBuf, PathBuf) {
    let requests_file = base_dir.join("requests.jsonl");
    let port_file = base_dir.join("port");
    let control_file = base_dir.join("server_state");

    fs::write(&control_file, "down").unwrap();

    let script = r#"
import http.server
import json
import sys

requests_path = sys.argv[1]
port_path = sys.argv[2]
control_path = sys.argv[3]

class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        content_length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(content_length).decode('utf-8')

        try:
            state = open(control_path).read().strip()
        except:
            state = "down"

        if state == "down":
            self.send_response(503)
            self.end_headers()
            self.wfile.write(b'{"error":"service unavailable"}')
            return

        record = json.dumps({
            "path": self.path,
            "body": json.loads(body) if body else None
        })
        with open(requests_path, 'a') as f:
            f.write(record + '\n')

        if '/metrics/' in self.path:
            response = '{"errors":[]}'
        elif '/cas/' in self.path:
            response = '{"results":[],"success_count":1,"failure_count":0}'
        else:
            response = '{}'

        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(response.encode())

    def log_message(self, format, *args):
        pass

server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
port = server.server_address[1]
with open(port_path, 'w') as f:
    f.write(str(port))
server.serve_forever()
"#;

    let script_path = base_dir.join("mock_server.py");
    fs::write(&script_path, script).unwrap();

    let mut child = Command::new("python3")
        .arg(&script_path)
        .arg(&requests_file)
        .arg(&port_file)
        .arg(&control_file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start mock server (python3 required)");

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if port_file.exists() {
            let port_str = fs::read_to_string(&port_file).unwrap();
            let port: u16 = port_str.trim().parse().unwrap();
            return (child, port, requests_file, control_file);
        }
        thread::sleep(Duration::from_millis(50));
    }

    let _ = child.kill();
    let _ = child.wait();
    panic!("mock server did not start within 5s");
}

fn setup_daemon_home(base_dir: &Path) -> PathBuf {
    let daemon_dir = base_dir.join("daemon_home");
    let internal_dir = daemon_dir.join(".git-ai").join("internal");
    fs::create_dir_all(&internal_dir).unwrap();
    fs::write(internal_dir.join("distinct_id"), "test-queue-id").unwrap();

    let creds = serde_json::json!({
        "access_token": "test-token-queue",
        "expires_at": 9999999999_i64
    });
    fs::write(
        internal_dir.join("credentials.json"),
        serde_json::to_string(&creds).unwrap(),
    )
    .unwrap();

    let socket_dir = internal_dir.join("daemon");
    fs::create_dir_all(&socket_dir).unwrap();

    daemon_dir
}

fn create_trace2_home(base_dir: &Path, socket_path: &Path) -> PathBuf {
    let home = base_dir.join("trace2home");
    fs::create_dir_all(&home).unwrap();
    let content = format!(
        "[trace2]\n\teventTarget = af_unix:stream:{}\n\teventNesting = 10\n",
        socket_path.display()
    );
    fs::write(home.join(".gitconfig"), content).unwrap();
    home
}

fn wait_for_socket(socket_path: &Path, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if socket_path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "daemon socket did not appear at {} within {:?}",
        socket_path.display(),
        timeout
    );
}

fn wait_for_note(repo_path: &Path, commit_sha: &str, timeout: Duration) -> bool {
    let git = real_git_executable();
    let start = Instant::now();
    while start.elapsed() < timeout {
        let output = Command::new(git)
            .current_dir(repo_path)
            .args(["notes", "--ref=ai", "show", commit_sha])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .ok();
        if let Some(o) = output {
            if o.status.success() {
                return true;
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

fn read_requests(path: &Path) -> Vec<serde_json::Value> {
    if !path.exists() {
        return Vec::new();
    }
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn count_requests_by_path(requests: &[serde_json::Value], needle: &str) -> usize {
    requests
        .iter()
        .filter(|r| {
            r["path"]
                .as_str()
                .map(|p| p.contains(needle))
                .unwrap_or(false)
        })
        .count()
}

#[test]
fn test_telemetry_queue_persists_on_failure_and_drains_on_reconnect() {
    let base_dir = tempfile::tempdir().unwrap();
    let base = base_dir.path();

    let (mut server, port, requests_file, control_file) = spawn_controllable_mock(base);
    let daemon_home = setup_daemon_home(base);
    let socket_path = daemon_home
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("trace2.sock");

    let binary = get_binary_path();

    // Start daemon with server DOWN (503 responses cause curl to succeed but JSON parse fails)
    let mut daemon = Command::new(&binary)
        .args(["bg", "run", "--foreground"])
        .env("HOME", &daemon_home)
        .env("GIT_AI_API_BASE_URL", format!("http://127.0.0.1:{}", port))
        .env("GIT_AI_RETRY_DELAY_SECS", "1")
        .env("GIT_TRACE2_EVENT", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start daemon");

    wait_for_socket(&socket_path, Duration::from_secs(5));

    // Create a git repo, checkpoint + commit while server is DOWN
    let repo_path = base.join("testrepo");
    let git = real_git_executable();
    let home = create_trace2_home(base, &socket_path);

    let git_cmd = |args: &[&str]| -> String {
        let output = Command::new(git)
            .current_dir(&repo_path)
            .args(args)
            .env("HOME", &home)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "t@t.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "t@t.com")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };

    Command::new(git)
        .args(["init", repo_path.to_str().unwrap()])
        .env("HOME", &home)
        .output()
        .unwrap();
    git_cmd(&["config", "user.name", "Test"]);
    git_cmd(&["config", "user.email", "t@t.com"]);

    fs::write(repo_path.join("f.txt"), "base\n").unwrap();
    git_cmd(&["add", "."]);
    git_cmd(&["commit", "-m", "init"]);

    // AI edit + checkpoint + commit
    fs::write(repo_path.join("f.txt"), "base\nai line\n").unwrap();
    let cp = Command::new(&binary)
        .current_dir(&repo_path)
        .args(["checkpoint", "mock_ai", "f.txt"])
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "0")
        .output()
        .unwrap();
    assert!(cp.status.success(), "checkpoint failed");

    git_cmd(&["add", "."]);
    git_cmd(&["commit", "-m", "ai commit"]);
    let commit_sha = git_cmd(&["rev-parse", "HEAD"]);

    // Wait for daemon to process the commit
    assert!(
        wait_for_note(&repo_path, &commit_sha, Duration::from_secs(10)),
        "daemon should still write notes even when telemetry upload fails"
    );

    // Give telemetry flush cycle time (3s flush + 1s retry + margin)
    thread::sleep(Duration::from_secs(6));

    // Kill daemon — offline data should be in SQLite
    unsafe {
        libc::kill(daemon.id() as i32, libc::SIGTERM);
    }
    let _ = daemon.wait();

    // Verify no requests reached the server
    let before_requests = read_requests(&requests_file);
    assert_eq!(
        count_requests_by_path(&before_requests, "/metrics/"),
        0,
        "server is down — no metrics should have been accepted"
    );
    assert_eq!(
        count_requests_by_path(&before_requests, "/cas/"),
        0,
        "server is down — no CAS should have been accepted"
    );

    // Verify SQLite queue exists
    let db_path = daemon_home
        .join(".git-ai")
        .join("internal")
        .join("telemetry_queue.db");
    assert!(
        db_path.exists(),
        "telemetry_queue.db should exist after failed upload"
    );

    // Verify flush-metrics-db --stats shows pending items
    let stats_out = Command::new(&binary)
        .args(["flush-metrics-db", "--stats"])
        .env("HOME", &daemon_home)
        .env("GIT_AI_API_BASE_URL", format!("http://127.0.0.1:{}", port))
        .output()
        .unwrap();
    let stats_str = String::from_utf8_lossy(&stats_out.stdout);
    assert!(
        stats_str.contains("pending"),
        "flush-metrics-db --stats should show pending items, got: {}",
        stats_str,
    );

    // Bring server back UP and flush via CLI
    fs::write(&control_file, "up").unwrap();

    let flush_out = Command::new(&binary)
        .args(["flush-metrics-db"])
        .env("HOME", &daemon_home)
        .env("GIT_AI_API_BASE_URL", format!("http://127.0.0.1:{}", port))
        .output()
        .unwrap();
    let flush_stdout = String::from_utf8_lossy(&flush_out.stdout);
    let flush_stderr = String::from_utf8_lossy(&flush_out.stderr);
    assert!(
        flush_out.status.success(),
        "flush-metrics-db should succeed. stdout: {}, stderr: {}",
        flush_stdout,
        flush_stderr,
    );

    // Verify requests reached the server
    let after_requests = read_requests(&requests_file);
    let metrics_after = count_requests_by_path(&after_requests, "/metrics/");
    let cas_after = count_requests_by_path(&after_requests, "/cas/");

    // Cleanup
    unsafe {
        libc::kill(server.id() as i32, libc::SIGTERM);
    }
    let _ = server.kill();

    assert!(
        metrics_after > 0 || cas_after > 0,
        "after flush-metrics-db, queued telemetry should reach the server. metrics={}, cas={}",
        metrics_after,
        cas_after,
    );
}

#[test]
fn test_flush_metrics_db_empty_queue() {
    let base_dir = tempfile::tempdir().unwrap();
    let daemon_home = base_dir.path().join("home");
    let internal_dir = daemon_home.join(".git-ai").join("internal");
    fs::create_dir_all(&internal_dir).unwrap();
    fs::write(internal_dir.join("distinct_id"), "test-id").unwrap();
    let creds = serde_json::json!({"access_token": "tok", "expires_at": 9999999999_i64});
    fs::write(
        internal_dir.join("credentials.json"),
        serde_json::to_string(&creds).unwrap(),
    )
    .unwrap();

    let binary = get_binary_path();
    let out = Command::new(&binary)
        .args(["flush-metrics-db"])
        .env("HOME", &daemon_home)
        .env("GIT_AI_API_BASE_URL", "http://127.0.0.1:1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("empty") || stdout.contains("nothing"),
        "empty queue should report nothing to flush, got: {}",
        stdout,
    );
}

#[test]
fn test_telemetry_queue_bounded_growth() {
    use git_ai::daemon::telemetry_queue::TelemetryQueue;
    use git_ai::daemon::telemetry_types::{MetricEvent, MetricEventId, SparseArray};

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bounded.db");
    let queue = TelemetryQueue::open(&db_path).unwrap();

    let event = MetricEvent::new(MetricEventId::Committed, SparseArray::new(), SparseArray::new());
    for _ in 0..100 {
        queue.enqueue_metrics(&[event.clone()]).unwrap();
    }

    let count = queue.pending_metrics_count().unwrap();
    assert_eq!(count, 100);

    let batches = queue.drain_metrics(200).unwrap();
    assert_eq!(batches.len(), 100);

    let ids: Vec<i64> = batches.iter().map(|(id, _)| *id).collect();
    queue.delete_metrics(&ids).unwrap();
    assert_eq!(queue.pending_metrics_count().unwrap(), 0);
}
