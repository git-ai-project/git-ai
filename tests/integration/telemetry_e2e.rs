//! End-to-end test: daemon processes a commit and uploads telemetry to a mock server.
//!
//! Verifies the full path: checkpoint → commit → daemon detects → writes note → emits
//! metrics + CAS upload to backend. Uses a Python mock HTTP server to capture requests.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::repos::test_repo::{get_binary_path, real_git_executable};

/// Spawn a mock HTTP server that records request bodies to a file.
/// Returns (child process, port, requests_file_path).
fn spawn_mock_server(base_dir: &std::path::Path) -> (Child, u16, PathBuf) {
    let requests_file = base_dir.join("requests.jsonl");
    let port_file = base_dir.join("port");

    let script = r#"
import http.server
import json
import sys
import os

requests_path = sys.argv[1]
port_path = sys.argv[2]

class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        content_length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(content_length).decode('utf-8')
        record = json.dumps({
            "path": self.path,
            "headers": dict(self.headers),
            "body": json.loads(body) if body else None
        })
        with open(requests_path, 'a') as f:
            f.write(record + '\n')

        # Return appropriate response based on endpoint
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
        pass  # suppress logs

server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
port = server.server_address[1]
with open(port_path, 'w') as f:
    f.write(str(port))
server.serve_forever()
"#
    .to_string();

    let script_path = base_dir.join("mock_server.py");
    fs::write(&script_path, script).unwrap();

    let mut child = Command::new("python3")
        .arg(&script_path)
        .arg(&requests_file)
        .arg(&port_file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start mock server (python3 required)");

    // Wait for port file to appear
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if port_file.exists() {
            let port_str = fs::read_to_string(&port_file).unwrap();
            let port: u16 = port_str.trim().parse().unwrap();
            return (child, port, requests_file);
        }
        thread::sleep(Duration::from_millis(50));
    }

    // Child never started properly - kill and wait on it
    let _ = child.kill();
    let _ = child.wait();
    panic!("mock server did not start within 5s");
}

/// Create a HOME directory with trace2 config pointing to the daemon socket.
fn create_trace2_home(base_dir: &std::path::Path, socket_path: &Path) -> PathBuf {
    let home = base_dir.join("trace2home");
    fs::create_dir_all(&home).unwrap();
    let gitconfig = home.join(".gitconfig");
    let content = format!(
        "[trace2]\n\teventTarget = af_unix:stream:{}\n\teventNesting = 10\n",
        socket_path.display()
    );
    fs::write(&gitconfig, content).unwrap();
    home
}

/// Wait for a file to have at least `min_lines` lines.
fn wait_for_lines(path: &PathBuf, min_lines: usize, timeout: Duration) -> Vec<String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists()
            && let Ok(content) = fs::read_to_string(path)
        {
            let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
            if lines.len() >= min_lines {
                return lines;
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    if path.exists() {
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|s| s.to_string())
            .collect()
    } else {
        Vec::new()
    }
}

/// Wait for the daemon to write an authorship note on a commit.
fn wait_for_note(repo_path: &PathBuf, commit_sha: &str, timeout: Duration) -> Option<String> {
    let git = real_git_executable();
    let start = Instant::now();
    while start.elapsed() < timeout {
        let output = Command::new(git)
            .current_dir(repo_path)
            .args(["notes", "--ref=ai", "show", commit_sha])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .ok()?;

        if output.status.success() {
            let note = String::from_utf8_lossy(&output.stdout).to_string();
            if !note.trim().is_empty() {
                return Some(note);
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    None
}

#[test]
fn test_daemon_telemetry_uploads_on_commit() {
    let base_dir = tempfile::tempdir().unwrap();
    let base = base_dir.path();

    // Start mock HTTP server
    let (mut server, port, requests_file) = spawn_mock_server(base);

    // Start the daemon with GIT_AI_API_BASE_URL pointing to our mock
    let binary = get_binary_path();
    let daemon_dir = base.join("daemon_home");
    fs::create_dir_all(&daemon_dir).unwrap();
    let internal_dir = daemon_dir.join(".git-ai").join("internal");
    fs::create_dir_all(&internal_dir).unwrap();

    // Write a distinct_id so the daemon doesn't try /dev/urandom race
    fs::write(internal_dir.join("distinct_id"), "test-distinct-id-1234").unwrap();

    // Write credentials so should_upload() returns true
    let creds = serde_json::json!({
        "access_token": "test-token-abc",
        "expires_at": 9999999999_i64
    });
    fs::write(
        internal_dir.join("credentials.json"),
        serde_json::to_string(&creds).unwrap(),
    )
    .unwrap();

    let socket_path = internal_dir.join("daemon").join("trace2.sock");
    fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

    let mut daemon = Command::new(binary)
        .args(["bg", "run", "--foreground"])
        .env("HOME", &daemon_dir)
        .env("GIT_AI_API_BASE_URL", format!("http://127.0.0.1:{}", port))
        .env("GIT_TRACE2_EVENT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start daemon");

    // Wait for socket to appear
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if socket_path.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        socket_path.exists(),
        "daemon socket did not appear at {}",
        socket_path.display()
    );

    // Create a git repo and configure trace2 to point to daemon
    let repo_path = base.join("testrepo");
    let git = real_git_executable();
    let home = create_trace2_home(base, &socket_path);

    let git_cmd = |args: &[&str]| -> String {
        let output = Command::new(git)
            .current_dir(&repo_path)
            .args(args)
            .env("HOME", &home)
            .env("GIT_AUTHOR_NAME", "Test User")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test User")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };

    // Init repo
    Command::new(git)
        .args(["init", repo_path.to_str().unwrap()])
        .env("HOME", &home)
        .output()
        .unwrap();
    git_cmd(&["config", "user.name", "Test User"]);
    git_cmd(&["config", "user.email", "test@example.com"]);

    // Initial commit (no attribution)
    fs::write(repo_path.join("file.txt"), "line 1\n").unwrap();
    git_cmd(&["add", "."]);
    git_cmd(&["commit", "-m", "initial"]);
    let _initial_sha = git_cmd(&["rev-parse", "HEAD"]);

    // Checkpoint + commit (with AI attribution)
    fs::write(repo_path.join("file.txt"), "line 1\nAI line\n").unwrap();

    let checkpoint_output = Command::new(binary)
        .current_dir(&repo_path)
        .args(["checkpoint", "mock_ai", "file.txt"])
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        checkpoint_output.status.success(),
        "checkpoint failed: {}",
        String::from_utf8_lossy(&checkpoint_output.stderr)
    );

    git_cmd(&["add", "."]);
    git_cmd(&["commit", "-m", "add AI line"]);
    let commit_sha = git_cmd(&["rev-parse", "HEAD"]);

    // Wait for daemon to write the note
    let note = wait_for_note(&repo_path, &commit_sha, Duration::from_secs(10));
    assert!(
        note.is_some(),
        "daemon did not write authorship note within 10s"
    );

    // Wait for telemetry flush (3s flush interval + buffer for both metrics and CAS)
    let lines = wait_for_lines(&requests_file, 2, Duration::from_secs(12));

    // Shut down daemon and server
    unsafe {
        libc::kill(daemon.id() as i32, libc::SIGTERM);
    }
    let _ = daemon.wait();
    unsafe {
        libc::kill(server.id() as i32, libc::SIGTERM);
    }
    let _ = server.kill();

    // Verify we got at least a metrics upload and a CAS upload
    assert!(
        !lines.is_empty(),
        "no telemetry requests received by mock server"
    );

    let mut found_metrics = false;
    let mut found_cas = false;

    for line in &lines {
        let req: serde_json::Value = serde_json::from_str(line).unwrap();
        let path = req["path"].as_str().unwrap_or("");

        if path.contains("/metrics/") {
            found_metrics = true;
            let body = &req["body"];
            // Verify metrics batch structure
            assert_eq!(body["v"], 1, "metrics batch version should be 1");
            assert!(body["events"].is_array(), "events should be an array");
            let events = body["events"].as_array().unwrap();
            assert!(!events.is_empty(), "should have at least one event");

            // First event should be Committed (event_id = 1)
            let event = &events[0];
            assert_eq!(event["e"], 1, "event_id should be Committed (1)");
            assert!(event["t"].is_number(), "timestamp should be a number");
            assert!(event["v"].is_object(), "values should be an object");
            assert!(event["a"].is_object(), "attrs should be an object");

            // Attrs should contain version
            let attrs = event["a"].as_object().unwrap();
            assert!(
                attrs.contains_key("0"),
                "attrs should have version at position 0"
            );

            // Verify headers (case-insensitive lookup)
            let headers = req["headers"].as_object().unwrap();
            let auth_value = headers
                .iter()
                .find(|(k, _)| k.to_lowercase() == "authorization")
                .map(|(_, v)| v.as_str().unwrap_or(""))
                .unwrap_or("");
            assert!(
                auth_value.contains("Bearer test-token-abc"),
                "should send auth token, got: '{}'",
                auth_value
            );
            let distinct_value = headers
                .iter()
                .find(|(k, _)| k.to_lowercase() == "x-distinct-id")
                .map(|(_, v)| v.as_str().unwrap_or(""))
                .unwrap_or("");
            assert_eq!(
                distinct_value, "test-distinct-id-1234",
                "should send distinct ID"
            );
        }

        if path.contains("/cas") {
            found_cas = true;
            let body = &req["body"];
            // Verify CAS upload structure
            assert!(body["objects"].is_array(), "should have objects array");
            let objects = body["objects"].as_array().unwrap();
            assert!(!objects.is_empty(), "should have at least one CAS object");

            let obj = &objects[0];
            assert!(obj["content"].is_object(), "CAS content should be object");
            assert!(obj["hash"].is_string(), "CAS hash should be string");
            let hash = obj["hash"].as_str().unwrap();
            assert_eq!(hash.len(), 64, "hash should be SHA256 (64 hex chars)");
            assert!(
                hash.chars().all(|c| c.is_ascii_hexdigit()),
                "hash should be hex"
            );

            // Content should be a wrapped authorship note
            let content = &obj["content"];
            assert_eq!(
                content["type"].as_str().unwrap_or(""),
                "authorship_note",
                "CAS content type should be authorship_note"
            );
            assert!(
                content["raw"].is_string(),
                "CAS content should have raw note text"
            );

            // Metadata should contain commit SHA
            if let Some(metadata) = obj.get("metadata")
                && let Some(commit) = metadata.get("commit")
            {
                assert_eq!(
                    commit.as_str().unwrap_or(""),
                    &commit_sha,
                    "CAS metadata should reference the commit"
                );
            }
        }
    }

    assert!(
        found_metrics,
        "should have received a metrics upload request"
    );
    assert!(found_cas, "should have received a CAS upload request");
}
