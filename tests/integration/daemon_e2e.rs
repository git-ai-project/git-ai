use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::repos::test_repo::{get_binary_path, real_git_executable};

/// Read from a stream, retrying on EINTR (which happens when SIGCHLD interrupts a read).
fn read_retry(stream: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match stream.read(buf) {
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

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

/// Create a HOME directory with a .gitconfig that routes trace2 events to the socket.
/// Returns the HOME path. Git commands run with this HOME will send trace2 events.
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
fn test_daemon_detects_commit_and_writes_note() {
    let binary = get_binary_path();
    let git = real_git_executable();

    // Create an isolated daemon home directory
    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let socket_path = daemon_base.join("trace2.sock");
    let pid_file = daemon_base.join("daemon.pid.json");

    // Start the daemon in foreground mode with custom HOME
    let mut daemon_proc = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", daemon_home.path())
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn daemon");

    // Wait for the daemon to start (PID file appears)
    assert!(
        wait_for_file(&pid_file, Duration::from_secs(5)),
        "daemon did not start within 5s"
    );

    // Wait for the socket to appear
    assert!(
        wait_for_file(&socket_path, Duration::from_secs(5)),
        "daemon socket did not appear within 5s"
    );

    // Create a test repo
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_path = repo_dir.path().to_path_buf();

    let init_output = Command::new(git)
        .args(["init", repo_path.to_str().unwrap()])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    assert!(init_output.status.success(), "git init failed");

    // Configure the repo
    for args in [
        vec!["config", "user.name", "Test User"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        Command::new(git)
            .current_dir(&repo_path)
            .args(&args)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
    }

    // Create an initial commit so HEAD exists for checkpoint
    let init_file = repo_path.join("init.txt");
    fs::write(&init_file, "init\n").unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "initial"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Write a file and create an AI checkpoint
    let test_file = repo_path.join("hello.txt");
    fs::write(&test_file, "Hello from AI\n").unwrap();

    // Run git-ai checkpoint to mark this as AI-authored
    let cp_output = Command::new(binary)
        .current_dir(&repo_path)
        .args(["checkpoint", "mock_ai", "hello.txt"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .expect("checkpoint failed");
    assert!(
        cp_output.status.success(),
        "checkpoint failed: {}",
        String::from_utf8_lossy(&cp_output.stderr)
    );

    // Route trace2 events to daemon via HOME/.gitconfig.
    // GIT_TRACE2_EVENT env var does NOT support af_unix: targets —
    // only trace2.eventTarget in ~/.gitconfig (read via HOME) works.
    let trace2_home = create_trace2_home(daemon_home.path(), &socket_path);

    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "hello.txt"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .env_remove("GIT_TRACE2_EVENT_TARGET")
        .output()
        .unwrap();

    // Print env vars that might affect trace2
    for (k, v) in std::env::vars() {
        if k.contains("GIT") || k.contains("TRACE") {
            eprintln!("[test-env] {}={}", k, v);
        }
    }

    // Verify git can see the trace2 config
    let config_check = Command::new(git)
        .current_dir(&repo_path)
        .args(["config", "--list", "--show-origin"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();
    let config_output = String::from_utf8_lossy(&config_check.stdout);
    for line in config_output.lines() {
        if line.contains("trace2") {
            eprintln!("[test] {}", line);
        }
    }
    eprintln!("[test] trace2_home: {}", trace2_home.display());
    eprintln!(
        "[test] .gitconfig exists: {}",
        trace2_home.join(".gitconfig").exists()
    );
    eprintln!(
        "[test] .gitconfig content: {}",
        fs::read_to_string(trace2_home.join(".gitconfig")).unwrap_or_default()
    );

    let commit_output = Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "AI commit"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .env_remove("GIT_TRACE2_EVENT_TARGET")
        .output()
        .unwrap();
    assert!(
        commit_output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&commit_output.stderr)
    );
    eprintln!(
        "[test] commit output: {}",
        String::from_utf8_lossy(&commit_output.stdout).trim()
    );

    // Get the commit SHA
    let head_output = Command::new(git)
        .current_dir(&repo_path)
        .args(["rev-parse", "HEAD"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let commit_sha = String::from_utf8_lossy(&head_output.stdout)
        .trim()
        .to_string();

    // Wait for the daemon to process the commit and write the note
    let note = wait_for_note(&repo_path, &commit_sha, Duration::from_secs(10));

    // Read the daemon log file (stderr is redirected there)
    let log_file = daemon_base.join("daemon.log");
    let daemon_log = fs::read_to_string(&log_file).unwrap_or_default();

    // Kill the daemon
    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    // Verify the note was written
    assert!(
        note.is_some(),
        "daemon did not write authorship note within 10s for commit {}\ndaemon log:\n{}",
        &commit_sha[..7],
        daemon_log
    );

    let note_content = note.unwrap();
    assert!(
        note_content.contains("authorship/3.0.0"),
        "note doesn't contain expected schema version: {}",
        &note_content[..200.min(note_content.len())]
    );
}

#[test]
fn test_daemon_is_idempotent() {
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
    let pid_file = daemon_base.join("daemon.pid.json");

    // Start daemon
    let mut daemon_proc = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", daemon_home.path())
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn daemon");

    assert!(wait_for_file(&pid_file, Duration::from_secs(5)));
    assert!(wait_for_file(&socket_path, Duration::from_secs(5)));

    let trace2_home = create_trace2_home(daemon_home.path(), &socket_path);

    // Create test repo
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_path = repo_dir.path().to_path_buf();

    Command::new(git)
        .args(["init", repo_path.to_str().unwrap()])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    for args in [
        vec!["config", "user.name", "Test User"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        Command::new(git)
            .current_dir(&repo_path)
            .args(&args)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
    }

    // Initial commit
    fs::write(repo_path.join("init.txt"), "init\n").unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "initial"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Write a file with checkpoint
    fs::write(repo_path.join("hello.txt"), "Hello\n").unwrap();
    Command::new(binary)
        .current_dir(&repo_path)
        .args(["checkpoint", "mock_ai", "hello.txt"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Commit with trace2 going to daemon
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "Test idempotency"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();

    // Get commit SHA
    let head = Command::new(git)
        .current_dir(&repo_path)
        .args(["rev-parse", "HEAD"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let commit_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // Manually run post-commit (simulating the hook firing too)
    Command::new(binary)
        .current_dir(&repo_path)
        .args(["post-commit"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Read the note that the hook wrote
    let note_before = Command::new(git)
        .current_dir(&repo_path)
        .args(["notes", "--ref=ai", "show", &commit_sha])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let note_before_content = String::from_utf8_lossy(&note_before.stdout).to_string();

    // Wait for the daemon to have had time to process (it should skip)
    thread::sleep(Duration::from_secs(3));

    // Read the note again — it should be identical (daemon skipped it)
    let note_after = Command::new(git)
        .current_dir(&repo_path)
        .args(["notes", "--ref=ai", "show", &commit_sha])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let note_after_content = String::from_utf8_lossy(&note_after.stdout).to_string();

    // Kill daemon
    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    assert_eq!(
        note_before_content, note_after_content,
        "daemon should not overwrite an existing note"
    );
}

#[test]
fn test_daemon_handles_rapid_sequential_commits() {
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
    let pid_file = daemon_base.join("daemon.pid.json");

    // Start daemon
    let mut daemon_proc = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", daemon_home.path())
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn daemon");

    assert!(wait_for_file(&pid_file, Duration::from_secs(5)));
    assert!(wait_for_file(&socket_path, Duration::from_secs(5)));

    let trace2_home = create_trace2_home(daemon_home.path(), &socket_path);

    // Create test repo
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_path = repo_dir.path().to_path_buf();

    Command::new(git)
        .args(["init", repo_path.to_str().unwrap()])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.name", "Test User"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        Command::new(git)
            .current_dir(&repo_path)
            .args(&args)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
    }

    // Initial commit so HEAD exists for checkpoints
    fs::write(repo_path.join("init.txt"), "init\n").unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "initial"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Make 3 rapid commits
    let mut commit_shas = Vec::new();
    for i in 1..=3 {
        let file = repo_path.join(format!("file{}.txt", i));
        fs::write(&file, format!("Content {}\n", i)).unwrap();

        Command::new(binary)
            .current_dir(&repo_path)
            .args(["checkpoint", "mock_ai", &format!("file{}.txt", i)])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();

        Command::new(git)
            .current_dir(&repo_path)
            .args(["add", "-A"])
            .env("HOME", &trace2_home)
            .env_remove("GIT_TRACE2_EVENT")
            .output()
            .unwrap();

        Command::new(git)
            .current_dir(&repo_path)
            .args(["commit", "-m", &format!("Commit {}", i)])
            .env("HOME", &trace2_home)
            .env_remove("GIT_TRACE2_EVENT")
            .output()
            .unwrap();

        let head = Command::new(git)
            .current_dir(&repo_path)
            .args(["rev-parse", "HEAD"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
        commit_shas.push(String::from_utf8_lossy(&head.stdout).trim().to_string());
    }

    // Wait for all notes to appear
    let mut all_noted = true;
    for sha in &commit_shas {
        if wait_for_note(&repo_path, sha, Duration::from_secs(15)).is_none() {
            all_noted = false;
            eprintln!("note missing for commit {}", &sha[..7]);
        }
    }

    // Kill daemon
    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    assert!(
        all_noted,
        "daemon should write notes for all 3 rapid commits"
    );
}

#[test]
fn test_checkpoint_via_control_socket() {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;

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

    // Start daemon
    let mut daemon_proc = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", daemon_home.path())
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn daemon");

    assert!(
        wait_for_file(&pid_file, Duration::from_secs(5)),
        "daemon did not start"
    );
    assert!(
        wait_for_file(&control_path, Duration::from_secs(5)),
        "control socket did not appear"
    );

    let trace2_home = create_trace2_home(daemon_home.path(), &socket_path);

    // Create test repo
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_path = repo_dir.path().to_path_buf();

    Command::new(git)
        .args(["init", repo_path.to_str().unwrap()])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.name", "Test User"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        Command::new(git)
            .current_dir(&repo_path)
            .args(&args)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
    }

    // Initial commit
    fs::write(repo_path.join("init.txt"), "init\n").unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "initial"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Write a file
    fs::write(repo_path.join("hello.txt"), "Hello from control socket\n").unwrap();

    // Send checkpoint via control socket (instead of CLI)
    let mut client = UnixStream::connect(&control_path).expect("connect to control socket failed");
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();

    let request = format!(
        r#"{{"type":"checkpoint","repo_dir":"{}","kind":"ai","files":[{{"path":"hello.txt"}}],"agent":{{"tool":"test-agent","id":"session-ctrl-1","model":"test-model"}}}}"#,
        repo_path.display()
    );
    writeln!(client, "{}", request).unwrap();
    client.flush().unwrap();

    let mut buf = [0u8; 4096];
    let n = read_retry(&mut client, &mut buf).unwrap();
    let response: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&buf[..n]).trim()).unwrap();

    assert_eq!(response["ok"], true, "checkpoint response: {:?}", response);
    assert_eq!(
        response["processed"], 1,
        "expected 1 file processed: {:?}",
        response
    );

    // Now commit with trace2 going to daemon
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "hello.txt"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();

    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "Control socket commit"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();

    // Get commit SHA
    let head = Command::new(git)
        .current_dir(&repo_path)
        .args(["rev-parse", "HEAD"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let commit_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // Wait for note
    let note = wait_for_note(&repo_path, &commit_sha, Duration::from_secs(10));

    // Kill daemon
    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    // Verify note
    let note_content =
        note.expect("daemon did not write note for control-socket checkpoint commit");
    assert!(
        note_content.contains("authorship/3.0.0"),
        "note missing schema version"
    );
    assert!(
        note_content.contains("test-agent"),
        "note should reference test-agent: {}",
        &note_content[..200.min(note_content.len())]
    );
}

#[test]
fn test_control_socket_status() {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;

    let binary = get_binary_path();
    let git = real_git_executable();

    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

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

    // Create test repo
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_path = repo_dir.path().to_path_buf();

    Command::new(git)
        .args(["init", repo_path.to_str().unwrap()])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "t@t.com"],
    ] {
        Command::new(git)
            .current_dir(&repo_path)
            .args(&args)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
    }
    fs::write(repo_path.join("init.txt"), "init\n").unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "initial"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Query status before any checkpoint — should show 0 checkpoints
    let mut client = UnixStream::connect(&control_path).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let req = format!(
        r#"{{"type":"status","repo_dir":"{}"}}"#,
        repo_path.display()
    );
    writeln!(client, "{}", req).unwrap();
    client.flush().unwrap();

    let mut buf = [0u8; 4096];
    let n = read_retry(&mut client, &mut buf).unwrap();
    let resp: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&buf[..n]).trim()).unwrap();

    assert_eq!(resp["ok"], true);
    assert_eq!(resp["status"]["checkpoint_count"], 0);

    // Write a file and send checkpoint
    fs::write(repo_path.join("test.txt"), "content\n").unwrap();
    drop(client);

    let mut client2 = UnixStream::connect(&control_path).unwrap();
    client2
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let cp_req = format!(
        r#"{{"type":"checkpoint","repo_dir":"{}","kind":"ai","files":[{{"path":"test.txt"}}],"agent":{{"tool":"agent1"}}}}"#,
        repo_path.display()
    );
    writeln!(client2, "{}", cp_req).unwrap();
    client2.flush().unwrap();
    let n = read_retry(&mut client2, &mut buf).unwrap();
    let cp_resp: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&buf[..n]).trim()).unwrap();
    assert_eq!(cp_resp["ok"], true);
    assert_eq!(cp_resp["processed"], 1);

    // Query status again — should show 1 checkpoint
    drop(client2);
    let mut client3 = UnixStream::connect(&control_path).unwrap();
    client3
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(client3, "{}", req).unwrap();
    client3.flush().unwrap();
    let n = read_retry(&mut client3, &mut buf).unwrap();
    let resp2: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&buf[..n]).trim()).unwrap();

    assert_eq!(resp2["status"]["checkpoint_count"], 1);
    assert_eq!(resp2["status"]["files"][0], "test.txt");

    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();
}

#[test]
fn test_daemon_rewrite_amend_copies_note() {
    let binary = get_binary_path();
    let git = real_git_executable();

    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let socket_path = daemon_base.join("trace2.sock");
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
    assert!(wait_for_file(&socket_path, Duration::from_secs(5)));

    let trace2_home = create_trace2_home(daemon_home.path(), &socket_path);

    // Create test repo
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_path = repo_dir.path().to_path_buf();

    Command::new(git)
        .args(["init", repo_path.to_str().unwrap()])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "t@t.com"],
    ] {
        Command::new(git)
            .current_dir(&repo_path)
            .args(&args)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
    }

    // Initial commit
    fs::write(repo_path.join("init.txt"), "init\n").unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "initial"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Create a commit with checkpoint + authorship note
    fs::write(repo_path.join("hello.txt"), "Hello AI\n").unwrap();
    Command::new(binary)
        .current_dir(&repo_path)
        .args(["checkpoint", "mock_ai", "hello.txt"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "original commit"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();

    // Get original commit SHA and wait for note
    let head = Command::new(git)
        .current_dir(&repo_path)
        .args(["rev-parse", "HEAD"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let orig_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();

    let orig_note = wait_for_note(&repo_path, &orig_sha, Duration::from_secs(10));
    assert!(
        orig_note.is_some(),
        "daemon did not write note for original commit"
    );

    // Now amend the commit (with trace2 going to daemon)
    fs::write(repo_path.join("hello.txt"), "Hello AI amended\n").unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "hello.txt"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "--amend", "-m", "amended commit"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();

    // Get new HEAD SHA
    let head = Command::new(git)
        .current_dir(&repo_path)
        .args(["rev-parse", "HEAD"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let new_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
    assert_ne!(orig_sha, new_sha, "amend should create a new commit SHA");

    // Wait for the daemon to propagate the note to the amended commit
    let new_note = wait_for_note(&repo_path, &new_sha, Duration::from_secs(10));

    let log_file = daemon_base.join("daemon.log");
    let daemon_log = fs::read_to_string(&log_file).unwrap_or_default();

    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    assert!(
        new_note.is_some(),
        "daemon did not propagate note after amend\ndaemon log:\n{}",
        daemon_log
    );
    let note_content = new_note.unwrap();
    assert!(
        note_content.contains("authorship/3.0.0"),
        "propagated note should contain schema version"
    );
}

#[test]
fn test_daemon_rewrite_rebase_copies_notes() {
    let binary = get_binary_path();
    let git = real_git_executable();

    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let socket_path = daemon_base.join("trace2.sock");
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
    assert!(wait_for_file(&socket_path, Duration::from_secs(5)));

    let trace2_home = create_trace2_home(daemon_home.path(), &socket_path);

    // Create test repo
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_path = repo_dir.path().to_path_buf();

    Command::new(git)
        .args(["init", repo_path.to_str().unwrap()])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "t@t.com"],
    ] {
        Command::new(git)
            .current_dir(&repo_path)
            .args(&args)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
    }

    // Initial commit on master
    fs::write(repo_path.join("base.txt"), "base\n").unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "base"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Create feature branch with a noted commit
    Command::new(git)
        .current_dir(&repo_path)
        .args(["checkout", "-b", "feature"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    fs::write(repo_path.join("feat.txt"), "feature line\n").unwrap();
    Command::new(binary)
        .current_dir(&repo_path)
        .args(["checkpoint", "mock_ai", "feat.txt"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "feature commit"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();

    let head = Command::new(git)
        .current_dir(&repo_path)
        .args(["rev-parse", "HEAD"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let feature_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // Wait for the note on the feature commit
    let orig_note = wait_for_note(&repo_path, &feature_sha, Duration::from_secs(10));
    assert!(
        orig_note.is_some(),
        "daemon did not write note for feature commit"
    );

    // Add a commit on master to create divergence
    Command::new(git)
        .current_dir(&repo_path)
        .args(["checkout", "master"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    fs::write(repo_path.join("main.txt"), "main line\n").unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "main commit"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Rebase feature onto master (with trace2 events going to daemon)
    Command::new(git)
        .current_dir(&repo_path)
        .args(["checkout", "feature"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["rebase", "master"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();

    // Get new HEAD after rebase
    let head = Command::new(git)
        .current_dir(&repo_path)
        .args(["rev-parse", "HEAD"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let new_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
    assert_ne!(
        feature_sha, new_sha,
        "rebase should have created a new commit"
    );

    // Wait for the daemon to copy the note to the rebased commit
    let new_note = wait_for_note(&repo_path, &new_sha, Duration::from_secs(10));

    let log_file = daemon_base.join("daemon.log");
    let daemon_log = fs::read_to_string(&log_file).unwrap_or_default();

    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    assert!(
        new_note.is_some(),
        "daemon did not propagate note after rebase\ndaemon log:\n{}",
        daemon_log
    );
    let note_content = new_note.unwrap();
    assert!(
        note_content.contains("authorship/3.0.0"),
        "propagated note should contain schema version"
    );
}

#[test]
fn test_daemon_handles_concurrent_multi_repo_commits() {
    let binary = get_binary_path();
    let git = real_git_executable();

    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let socket_path = daemon_base.join("trace2.sock");
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
    assert!(wait_for_file(&socket_path, Duration::from_secs(5)));

    let trace2_home = create_trace2_home(daemon_home.path(), &socket_path);

    // Helper to initialize a repo
    let init_repo = |name: &str| -> PathBuf {
        let repo_dir = daemon_home.path().join(name);
        fs::create_dir_all(&repo_dir).unwrap();

        Command::new(git)
            .args(["init", repo_dir.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
        for args in [
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "t@t.com"],
        ] {
            Command::new(git)
                .current_dir(&repo_dir)
                .args(&args)
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .unwrap();
        }
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

    // Create 3 independent repos
    let repo_a = init_repo("repo-a");
    let repo_b = init_repo("repo-b");
    let repo_c = init_repo("repo-c");

    // Create checkpoints and commits in all 3 repos rapidly
    let mut commit_shas: Vec<(PathBuf, String)> = Vec::new();
    for (repo, label) in [(&repo_a, "a"), (&repo_b, "b"), (&repo_c, "c")] {
        let filename = format!("{}.txt", label);
        fs::write(
            repo.join(&filename),
            format!("Content from repo {}\n", label),
        )
        .unwrap();

        Command::new(binary)
            .current_dir(repo)
            .args(["checkpoint", "mock_ai", &filename])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();

        Command::new(git)
            .current_dir(repo)
            .args(["add", "-A"])
            .env("HOME", &trace2_home)
            .env_remove("GIT_TRACE2_EVENT")
            .output()
            .unwrap();

        Command::new(git)
            .current_dir(repo)
            .args(["commit", "-m", &format!("Commit in repo {}", label)])
            .env("HOME", &trace2_home)
            .env_remove("GIT_TRACE2_EVENT")
            .output()
            .unwrap();

        let head = Command::new(git)
            .current_dir(repo)
            .args(["rev-parse", "HEAD"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
        let sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
        commit_shas.push((repo.clone(), sha));
    }

    // Wait for all notes to appear
    let mut all_noted = true;
    for (repo, sha) in &commit_shas {
        if wait_for_note(repo, sha, Duration::from_secs(15)).is_none() {
            all_noted = false;
            eprintln!(
                "note missing for commit {} in {}",
                &sha[..7],
                repo.display()
            );
        }
    }

    let log_file = daemon_base.join("daemon.log");
    let daemon_log = fs::read_to_string(&log_file).unwrap_or_default();

    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    assert!(
        all_noted,
        "daemon should write notes for commits across all 3 repos\ndaemon log:\n{}",
        daemon_log
    );
}

#[test]
fn test_daemon_resolves_symlinked_repo() {
    let binary = get_binary_path();
    let git = real_git_executable();

    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let socket_path = daemon_base.join("trace2.sock");
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
    assert!(wait_for_file(&socket_path, Duration::from_secs(5)));

    let trace2_home = create_trace2_home(daemon_home.path(), &socket_path);

    // Create a real repo
    let real_repo = daemon_home.path().join("real-repo");
    fs::create_dir_all(&real_repo).unwrap();

    Command::new(git)
        .args(["init", real_repo.to_str().unwrap()])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "t@t.com"],
    ] {
        Command::new(git)
            .current_dir(&real_repo)
            .args(&args)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
    }
    fs::write(real_repo.join("init.txt"), "init\n").unwrap();
    Command::new(git)
        .current_dir(&real_repo)
        .args(["add", "-A"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&real_repo)
        .args(["commit", "-m", "initial"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Create a symlink to the repo
    let link_path = daemon_home.path().join("linked-repo");
    std::os::unix::fs::symlink(&real_repo, &link_path).unwrap();

    // Checkpoint via the symlinked path
    fs::write(link_path.join("hello.txt"), "Hello via symlink\n").unwrap();
    Command::new(binary)
        .current_dir(&link_path)
        .args(["checkpoint", "mock_ai", "hello.txt"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Commit via symlinked path with trace2
    Command::new(git)
        .current_dir(&link_path)
        .args(["add", "-A"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&link_path)
        .args(["commit", "-m", "commit via symlink"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();

    // Get commit SHA (check via real path since that's where notes go)
    let head = Command::new(git)
        .current_dir(&real_repo)
        .args(["rev-parse", "HEAD"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let sha = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // Wait for note
    let note = wait_for_note(&real_repo, &sha, Duration::from_secs(10));

    let log_file = daemon_base.join("daemon.log");
    let daemon_log = fs::read_to_string(&log_file).unwrap_or_default();

    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    assert!(
        note.is_some(),
        "daemon should write note even when commit arrives via symlinked path\ndaemon log:\n{}",
        daemon_log
    );
    assert!(note.unwrap().contains("authorship/3.0.0"));
}

#[test]
fn test_daemon_stats_via_control_socket() {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;

    let binary = get_binary_path();

    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

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

    // Query stats
    let mut client = UnixStream::connect(&control_path).expect("connect failed");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(client, r#"{{"type":"stats"}}"#).unwrap();
    client.flush().unwrap();

    let mut buf = [0u8; 4096];
    let n = read_retry(&mut client, &mut buf).unwrap();
    let resp: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&buf[..n]).trim()).unwrap();

    assert_eq!(resp["ok"], true, "stats response should be ok: {:?}", resp);
    let stats_str = resp["stats"]
        .as_str()
        .expect("stats field should be a string");
    assert!(
        stats_str.contains("uptime:"),
        "stats should contain uptime: {}",
        stats_str
    );
    assert!(
        stats_str.contains("commits processed:"),
        "stats should contain commits processed"
    );

    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();
}
