use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::daemon::open_local_socket_stream_with_timeout;
#[cfg(not(windows))]
use git_ai::daemon::{ControlRequest, DaemonConfig, send_control_request_with_timeout};
#[cfg(not(windows))]
use interprocess::local_socket::{GenericFilePath, ListenerOptions, prelude::*};
use std::fs;
use std::io::Write;
#[cfg(not(windows))]
use std::io::{BufRead, BufReader};
#[cfg(not(windows))]
use std::thread;
use std::time::Duration;

#[cfg(not(windows))]
const MAX_CONTROL_RESPONSE_BYTES: usize = 36 * 1024 * 1024;

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

#[test]
#[cfg(not(windows))]
fn oversized_control_response_is_rejected_and_attribution_recovers() {
    let repo = TestRepo::new_dedicated_daemon();
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    let fake_home = repo.test_home_path().join("oversized-control-response");
    let socket_path = DaemonConfig::from_home(&fake_home).control_socket_path;
    fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
    let socket_name = socket_path
        .as_path()
        .to_fs_name::<GenericFilePath>()
        .expect("fake control socket path should be valid");
    let listener = ListenerOptions::new()
        .name(socket_name)
        .create_sync()
        .expect("fake control socket should bind");

    let server = thread::spawn(move || {
        let stream = listener
            .incoming()
            .next()
            .expect("fake control server should receive a connection")
            .expect("fake control connection should succeed");
        let mut stream = BufReader::new(stream);
        let mut request = String::new();
        stream
            .read_line(&mut request)
            .expect("fake control server should read a request");

        let chunk = [b'x'; 64 * 1024];
        let mut remaining = MAX_CONTROL_RESPONSE_BYTES + 1;
        while remaining > 0 {
            let write_len = remaining.min(chunk.len());
            if stream.get_mut().write_all(&chunk[..write_len]).is_err() {
                return;
            }
            remaining -= write_len;
        }
        let _ = stream.get_mut().write_all(b"\n");
        let _ = stream.get_mut().flush();
    });

    let error = send_control_request_with_timeout(
        &socket_path,
        &ControlRequest::Ping,
        Duration::from_secs(10),
    )
    .expect_err("oversized control response must be rejected before JSON parsing");
    assert!(
        error.to_string().contains("exceeds 37748736 bytes"),
        "unexpected oversized response error: {error}"
    );
    server.join().unwrap();

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after oversized control response")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
