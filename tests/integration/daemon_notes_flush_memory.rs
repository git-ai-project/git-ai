#![cfg(target_os = "linux")]

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::daemon::{ControlRequest, send_control_request_with_timeout};
use std::fs;
use std::time::Duration;

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
fn notes_flush_burst_keeps_threads_bounded_and_attribution_working() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_FLUSH_NOTES_DELAY_MS", "2000")]);
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    let baseline_threads = daemon_thread_count(&repo);
    for _ in 0..24 {
        let response = send_control_request_with_timeout(
            &repo.daemon_control_socket_path(),
            &ControlRequest::FlushNotes,
            Duration::from_secs(2),
        )
        .expect("notes flush request should complete");
        assert!(response.ok, "notes flush request should succeed");
    }
    std::thread::sleep(Duration::from_millis(250));

    let current_threads = daemon_thread_count(&repo);
    assert!(
        current_threads <= baseline_threads + 4,
        "notes flush burst grew daemon threads from {baseline_threads} to {current_threads}"
    );

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after notes flush pressure")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
