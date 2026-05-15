use std::fs;
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::repos::test_repo::{get_binary_path, real_git_executable};

/// Wait for a file to exist, with timeout.
fn wait_for_file(path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
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

/// Create a HOME directory with a .gitconfig that routes trace2 events to the socket.
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

/// Read from a stream, retrying on EINTR.
fn read_retry(stream: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match stream.read(buf) {
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

/// Stale socket cleanup: if a socket file exists from a crashed daemon,
/// `bg run` should clean it up and start fresh.
#[test]
fn test_stale_socket_cleanup() {
    let binary = get_binary_path();

    // Create an isolated daemon home directory
    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let socket_path = daemon_base.join("trace2.sock");
    let control_path = daemon_base.join("control.sock");
    let pid_file = daemon_base.join("daemon.pid.json");

    // Create stale socket files (simulating a crashed daemon)
    // These are just regular files, not real sockets, so they can't be connected to
    fs::write(&socket_path, "stale").unwrap();
    fs::write(&control_path, "stale").unwrap();

    // Also create a stale PID file referencing a dead process
    let stale_pid_content =
        r#"{"pid":999999999,"started_at":"2024-01-01T00:00:00Z","version":"0.0.0"}"#;
    fs::write(daemon_base.join("daemon.pid.json"), stale_pid_content).unwrap();

    // Start the daemon — it should clean up the stale files and start successfully
    let mut daemon_proc = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", daemon_home.path())
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn daemon");

    // Wait for the daemon to start
    assert!(
        wait_for_file(&pid_file, Duration::from_secs(10)),
        "daemon did not start within 10s (stale socket cleanup may have failed)"
    );

    // Verify the socket is now a real socket we can connect to
    assert!(
        wait_for_file(&socket_path, Duration::from_secs(5)),
        "trace2 socket did not appear after stale cleanup"
    );

    // Verify we can actually connect to the control socket
    let connected = {
        let start = Instant::now();
        let mut ok = false;
        while start.elapsed() < Duration::from_secs(5) {
            if UnixStream::connect(&control_path).is_ok() {
                ok = true;
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        ok
    };
    assert!(
        connected,
        "could not connect to control socket after stale cleanup"
    );

    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();
}

/// Concurrent repos: daemon handles trace2 events from multiple repos
/// simultaneously without mixing up working logs.
#[test]
fn test_concurrent_repos_isolation() {
    let binary = get_binary_path();
    let git = real_git_executable();

    // Create an isolated daemon home
    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let socket_path = daemon_base.join("trace2.sock");
    let control_path = daemon_base.join("control.sock");
    let pid_file = daemon_base.join("daemon.pid.json");

    let mut daemon_proc = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", daemon_home.path())
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn daemon");

    assert!(wait_for_file(&pid_file, Duration::from_secs(5)));
    assert!(wait_for_file(&control_path, Duration::from_secs(5)));

    let trace2_home = create_trace2_home(daemon_home.path(), &socket_path);

    // Helper to create and initialize a repo
    let create_repo = |name: &str| -> PathBuf {
        let repo_dir = daemon_home.path().join(name);
        fs::create_dir_all(&repo_dir).unwrap();

        Command::new(git)
            .args(["init", repo_dir.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
        for args in [
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.com"],
        ] {
            Command::new(git)
                .current_dir(&repo_dir)
                .args(&args)
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .unwrap();
        }
        // Initial commit
        fs::write(repo_dir.join("init.txt"), "init\n").unwrap();
        Command::new(git)
            .current_dir(&repo_dir)
            .args(["add", "-A"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
        Command::new(git)
            .current_dir(&repo_dir)
            .args(["commit", "-m", "initial"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
        repo_dir
    };

    let repo_alpha = create_repo("repo-alpha");
    let repo_beta = create_repo("repo-beta");

    // Checkpoint different files with different content in each repo
    fs::write(
        repo_alpha.join("alpha.txt"),
        "Alpha content line 1\nAlpha line 2\n",
    )
    .unwrap();
    fs::write(
        repo_beta.join("beta.txt"),
        "Beta content line 1\nBeta line 2\n",
    )
    .unwrap();

    // Send checkpoints via control socket for both repos (interleaved)
    let send_checkpoint = |repo_path: &PathBuf, filename: &str| {
        let mut client = UnixStream::connect(&control_path).expect("connect failed");
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let request = format!(
            r#"{{"type":"checkpoint","repo_dir":"{}","kind":"ai","files":[{{"path":"{}"}}],"agent":{{"tool":"test-agent","id":"session-1","model":"test-model"}}}}"#,
            repo_path.display(),
            filename
        );
        writeln!(client, "{}", request).unwrap();
        client.flush().unwrap();

        let mut buf = [0u8; 4096];
        let n = read_retry(&mut client, &mut buf).unwrap();
        let resp: serde_json::Value =
            serde_json::from_str(String::from_utf8_lossy(&buf[..n]).trim()).unwrap();
        assert_eq!(resp["ok"], true, "checkpoint failed: {:?}", resp);
    };

    send_checkpoint(&repo_alpha, "alpha.txt");
    send_checkpoint(&repo_beta, "beta.txt");

    // Commit in both repos (sequentially but rapidly)
    let commit_repo = |repo_path: &PathBuf, msg: &str| -> String {
        Command::new(git)
            .current_dir(repo_path)
            .args(["add", "-A"])
            .env("HOME", &trace2_home)
            .env_remove("GIT_TRACE2_EVENT")
            .output()
            .unwrap();
        Command::new(git)
            .current_dir(repo_path)
            .args(["commit", "-m", msg])
            .env("HOME", &trace2_home)
            .env_remove("GIT_TRACE2_EVENT")
            .output()
            .unwrap();
        let head = Command::new(git)
            .current_dir(repo_path)
            .args(["rev-parse", "HEAD"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
        String::from_utf8_lossy(&head.stdout).trim().to_string()
    };

    let sha_alpha = commit_repo(&repo_alpha, "Alpha commit");
    let sha_beta = commit_repo(&repo_beta, "Beta commit");

    // Wait for notes on both commits
    let note_alpha = wait_for_note(&repo_alpha, &sha_alpha, Duration::from_secs(15));
    let note_beta = wait_for_note(&repo_beta, &sha_beta, Duration::from_secs(15));

    let log_file = daemon_base.join("daemon.log");
    let daemon_log = fs::read_to_string(&log_file).unwrap_or_default();

    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    // Verify both repos got notes
    assert!(
        note_alpha.is_some(),
        "daemon did not write note for repo-alpha commit {}\ndaemon log:\n{}",
        &sha_alpha[..7],
        daemon_log
    );
    assert!(
        note_beta.is_some(),
        "daemon did not write note for repo-beta commit {}\ndaemon log:\n{}",
        &sha_beta[..7],
        daemon_log
    );

    let alpha_content = note_alpha.unwrap();
    let beta_content = note_beta.unwrap();

    // Verify the notes reference the correct files (no cross-contamination)
    assert!(
        alpha_content.contains("alpha.txt"),
        "alpha note should reference alpha.txt, got: {}",
        &alpha_content[..300.min(alpha_content.len())]
    );
    assert!(
        beta_content.contains("beta.txt"),
        "beta note should reference beta.txt, got: {}",
        &beta_content[..300.min(beta_content.len())]
    );

    // Verify no cross-contamination
    assert!(
        !alpha_content.contains("beta.txt"),
        "alpha note should NOT reference beta.txt"
    );
    assert!(
        !beta_content.contains("alpha.txt"),
        "beta note should NOT reference alpha.txt"
    );
}

/// Graceful shutdown: SIGTERM causes the daemon to flush pending state before exiting.
#[test]
fn test_graceful_shutdown_on_sigterm() {
    let binary = get_binary_path();

    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let pid_file = daemon_base.join("daemon.pid.json");
    let log_file = daemon_base.join("daemon.log");

    // Start daemon in foreground mode (bg run => foreground: true, no fork)
    let mut daemon_proc = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", daemon_home.path())
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn daemon");

    let child_pid = daemon_proc.id();

    assert!(
        wait_for_file(&pid_file, Duration::from_secs(5)),
        "daemon did not start"
    );

    // Send SIGTERM to the child process directly
    unsafe {
        libc::kill(child_pid as i32, libc::SIGTERM);
    }

    // Wait for daemon to exit gracefully using try_wait on the child handle
    let start = Instant::now();
    let mut exited = false;
    while start.elapsed() < Duration::from_secs(10) {
        match daemon_proc.try_wait() {
            Ok(Some(_status)) => {
                exited = true;
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        thread::sleep(Duration::from_millis(100));
    }

    if !exited {
        // Force kill if still running to avoid leaked processes
        let _ = daemon_proc.kill();
        let _ = daemon_proc.wait();
        panic!("daemon did not exit within 10s after SIGTERM");
    } else {
        // Ensure we wait on the exited process to avoid zombie
        let _ = daemon_proc.wait();
    }

    // Verify the daemon logged a clean shutdown message
    // Give a brief moment for the log file to be fully flushed
    thread::sleep(Duration::from_millis(200));
    let log_content = fs::read_to_string(&log_file).unwrap_or_default();
    assert!(
        log_content.contains("shutting down"),
        "daemon log should contain shutdown message, got:\n{}",
        log_content
    );

    // PID file should be cleaned up after graceful shutdown
    // (The daemon removes it in run_daemon after the event loop exits)
    assert!(
        !pid_file.exists(),
        "PID file should be removed after graceful shutdown"
    );
}

/// Double-start protection: if daemon is already running, starting another one
/// returns cleanly (doesn't crash or corrupt state).
#[test]
fn test_double_start_protection() {
    let binary = get_binary_path();

    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let pid_file = daemon_base.join("daemon.pid.json");
    let control_path = daemon_base.join("control.sock");

    // Start first daemon
    let mut daemon_proc_1 = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", daemon_home.path())
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn first daemon");

    assert!(
        wait_for_file(&pid_file, Duration::from_secs(5)),
        "first daemon did not start"
    );
    assert!(
        wait_for_file(&control_path, Duration::from_secs(5)),
        "first daemon control socket did not appear"
    );

    // Read the PID of the first daemon
    let pid_content_1 = fs::read_to_string(&pid_file).unwrap();

    // Try to start a second daemon with the same HOME
    let second_result = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", daemon_home.path())
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run second daemon attempt");

    // The second daemon should exit (either with error code or cleanly indicating already running)
    // It should NOT crash with a panic
    let stderr = String::from_utf8_lossy(&second_result.stderr);
    assert!(
        !stderr.contains("panic") && !stderr.contains("SIGSEGV"),
        "second daemon instance should not panic, got: {}",
        stderr
    );

    // The PID file should still reference the first daemon (not corrupted)
    let pid_content_after = fs::read_to_string(&pid_file).unwrap();
    assert_eq!(
        pid_content_1, pid_content_after,
        "PID file should not be corrupted by second start attempt"
    );

    // First daemon should still be responsive
    let connected = UnixStream::connect(&control_path).is_ok();
    assert!(
        connected,
        "first daemon should still be responsive after second start attempt"
    );

    let _ = daemon_proc_1.kill();
    let _ = daemon_proc_1.wait();
}

/// Socket path too long: when HOME path would make socket > 100 chars,
/// it hashes to /tmp (test resolve_trace2_socket_path logic).
#[test]
fn test_socket_path_too_long_uses_tmp() {
    let binary = get_binary_path();

    // Create a deeply nested HOME directory that would make the socket path > 100 chars
    // The socket path is: $HOME/.git-ai/internal/daemon/trace2.sock
    // That's about 36 chars of suffix, so we need HOME to be > 64 chars
    let base_tmp = tempfile::tempdir().unwrap();
    let long_segment = "a".repeat(80);
    let long_home = base_tmp
        .path()
        .join(&long_segment)
        .join("deep")
        .join("nested");
    fs::create_dir_all(&long_home).unwrap();

    // Verify our path is indeed > 100 chars for the socket
    let would_be_socket = long_home
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("trace2.sock");
    assert!(
        would_be_socket.to_string_lossy().len() >= 100,
        "test setup: socket path should be >= 100 chars, got {} chars: {}",
        would_be_socket.to_string_lossy().len(),
        would_be_socket.display()
    );

    let daemon_base = long_home.join(".git-ai").join("internal").join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let pid_file = daemon_base.join("daemon.pid.json");

    // Start daemon with the long HOME path
    let mut daemon_proc = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", &long_home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn daemon with long HOME");

    // Wait for the daemon to start
    assert!(
        wait_for_file(&pid_file, Duration::from_secs(10)),
        "daemon did not start with long HOME path"
    );

    // The socket should NOT be at the normal location (since it's too long)
    // It should be in /tmp/git-ai-d-<hash>/trace2.sock
    assert!(
        !would_be_socket.exists(),
        "socket should NOT be at the too-long path: {}",
        would_be_socket.display()
    );

    // Find the actual socket in /tmp
    let tmp_sockets: Vec<_> = fs::read_dir("/tmp")
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("git-ai-d-"))
        .collect();

    // There should be at least one /tmp/git-ai-d-* directory
    assert!(
        !tmp_sockets.is_empty(),
        "expected /tmp/git-ai-d-* directory for hashed socket path"
    );

    // Find the matching socket by checking which one has a trace2.sock
    let found_socket = tmp_sockets
        .iter()
        .any(|entry| entry.path().join("trace2.sock").exists());
    assert!(
        found_socket,
        "expected trace2.sock in one of the /tmp/git-ai-d-* directories"
    );

    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    // Cleanup /tmp entries we created
    for entry in &tmp_sockets {
        let _ = fs::remove_dir_all(entry.path());
    }
}
