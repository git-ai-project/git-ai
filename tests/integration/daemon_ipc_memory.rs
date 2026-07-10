use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::daemon::open_local_socket_stream_with_timeout;
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
fn stalled_trace_connections_respect_handler_limit_and_preserve_attribution() {
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
    let mut streams = Vec::new();
    for index in 0..32 {
        let Ok(mut stream) =
            open_local_socket_stream_with_timeout(&trace_socket, Duration::from_secs(2))
        else {
            continue;
        };
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
        let _ = stream.write_all(frames.as_bytes());
        let _ = stream.flush();
        streams.push(stream);
    }
    std::thread::sleep(Duration::from_millis(500));

    #[cfg(target_os = "linux")]
    assert!(
        daemon_thread_count(&repo) <= baseline_threads + 8,
        "trace handler thread count exceeded configured capacity"
    );
    drop(streams);

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after connection pressure")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
