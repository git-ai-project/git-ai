use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
#[cfg(not(windows))]
use crate::repos::test_repo::new_daemon_test_sync_session_id;
use git_ai::daemon::open_local_socket_stream_with_timeout;
#[cfg(not(windows))]
use git_ai::daemon::{ControlRequest, send_control_request_with_timeout};
use std::fs;
use std::io::Write;
use std::time::Duration;

#[cfg(target_os = "linux")]
fn daemon_hwm_kib(repo: &TestRepo) -> u64 {
    let pid = repo.daemon_pid().expect("test repo should own a daemon");
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmHWM:")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse().ok())
        })
        .expect("daemon status should include VmHWM")
}

#[cfg(target_os = "linux")]
fn daemon_thread_count(repo: &TestRepo) -> u64 {
    let pid = repo.daemon_pid().expect("test repo should own a daemon");
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("Threads:")
                .and_then(|value| value.trim().parse().ok())
        })
        .expect("daemon status should include Threads")
}

#[test]
fn oversized_trace_frame_keeps_daemon_bounded_and_attribution_working() {
    let repo = TestRepo::new_dedicated_daemon();
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);

    let mut stream = open_local_socket_stream_with_timeout(
        &repo.daemon_trace_socket_path(),
        Duration::from_secs(2),
    )
    .expect("connect trace socket");
    let mut frame = Vec::with_capacity(32 * 1024 * 1024);
    frame.extend_from_slice(br#"{"event":"start","sid":"oversized","padding":""#);
    frame.resize(32 * 1024 * 1024 - 3, b'x');
    frame.extend_from_slice(b"\"}\n");
    let _ = stream.write_all(&frame);
    let _ = stream.flush();
    drop(stream);
    std::thread::sleep(Duration::from_millis(250));

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        assert!(
            hwm_growth_kib < 24 * 1024,
            "oversized trace frame grew daemon HWM by {hwm_growth_kib} KiB"
        );
    }

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after oversized frame")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}

#[test]
#[cfg(not(windows))]
fn trace_connection_limit_backpressures_without_losing_commands() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_MAX_TRACE_CONNECTIONS", "4")]);
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    #[cfg(target_os = "linux")]
    let baseline_threads = daemon_thread_count(&repo);

    let trace_socket = repo.daemon_trace_socket_path();
    let worktree = repo.path().to_string_lossy();
    let git_dir = repo.path().join(".git").to_string_lossy().to_string();
    let mut stalled_streams = Vec::new();
    for index in 0..4 {
        let mut stream =
            open_local_socket_stream_with_timeout(&trace_socket, Duration::from_secs(2))
                .expect("connect stalled trace socket");
        let sid = format!("stalled-{index}");
        let frames = format!(
            "{}\n{}\n",
            serde_json::json!({
                "event": "start",
                "sid": sid,
                "argv": ["git", "status"],
                "worktree": worktree,
                "time_ns": index,
            }),
            serde_json::json!({
                "event": "def_repo",
                "sid": sid,
                "worktree": worktree,
                "repo": git_dir,
                "time_ns": index + 1,
            })
        );
        stream.write_all(frames.as_bytes()).unwrap();
        stream.flush().unwrap();
        stalled_streams.push(stream);
    }
    std::thread::sleep(Duration::from_millis(500));

    let sessions = (0..8)
        .map(|_| new_daemon_test_sync_session_id())
        .collect::<Vec<_>>();
    for (index, session) in sessions.iter().enumerate() {
        let mut stream =
            open_local_socket_stream_with_timeout(&trace_socket, Duration::from_secs(2))
                .expect("connect backpressured trace socket");
        let sid = format!("backpressured-{index}");
        let session_arg = format!("git-ai.testSyncSession={session}");
        let time_ns = 10_000 + index as u64 * 100;
        let frames = [
            serde_json::json!({
                "event": "start",
                "sid": sid,
                "argv": ["git", "-c", session_arg, "commit", "-m", "synthetic"],
                "time_ns": time_ns,
            }),
            serde_json::json!({
                "event": "def_repo",
                "sid": sid,
                "worktree": worktree,
                "repo": git_dir,
                "time_ns": time_ns + 1,
            }),
            serde_json::json!({
                "event": "exit",
                "sid": sid,
                "code": 0,
                "time_ns": time_ns + 2,
            }),
            serde_json::json!({
                "event": "atexit",
                "sid": sid,
                "code": 0,
                "time_ns": time_ns + 3,
            }),
        ]
        .into_iter()
        .map(|frame| serde_json::to_string(&frame).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
        let _ = stream.write_all(format!("{frames}\n").as_bytes());
        let _ = stream.flush();
    }

    #[cfg(target_os = "linux")]
    assert!(
        daemon_thread_count(&repo) <= baseline_threads + 8,
        "trace handler thread count exceeded configured capacity"
    );
    drop(stalled_streams);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let completed = repo
            .daemon_completion_entries()
            .into_iter()
            .filter_map(|entry| entry.test_sync_session)
            .collect::<std::collections::HashSet<_>>();
        let missing = sessions
            .iter()
            .filter(|session| !completed.contains(*session))
            .cloned()
            .collect::<Vec<_>>();
        if missing.is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "trace connection pressure dropped completion sessions: {missing:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    repo.sync_daemon_external_completion_sessions(&sessions);

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after connection pressure")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}

#[test]
#[cfg(not(windows))]
fn control_connection_limit_backpressures_without_dropping_requests() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_MAX_CONTROL_CONNECTIONS", "2")]);
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    #[cfg(target_os = "linux")]
    let baseline_threads = daemon_thread_count(&repo);

    let control_socket = repo.daemon_control_socket_path();
    let stalled_streams = (0..2)
        .map(|_| {
            open_local_socket_stream_with_timeout(&control_socket, Duration::from_secs(2))
                .expect("connect stalled control socket")
        })
        .collect::<Vec<_>>();
    std::thread::sleep(Duration::from_millis(250));

    let request_socket = control_socket.clone();
    let request = std::thread::spawn(move || {
        send_control_request_with_timeout(
            &request_socket,
            &ControlRequest::Ping,
            Duration::from_secs(5),
        )
    });
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        !request.is_finished(),
        "control request should remain backpressured while every handler is occupied"
    );

    #[cfg(target_os = "linux")]
    assert!(
        daemon_thread_count(&repo) <= baseline_threads + 4,
        "control handler thread count exceeded configured capacity"
    );
    drop(stalled_streams);

    let response = request
        .join()
        .expect("control request thread should not panic")
        .expect("backpressured control request should complete");
    assert!(
        response.ok,
        "backpressured ping should succeed: {response:?}"
    );

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after control pressure")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
