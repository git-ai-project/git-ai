#![cfg(target_os = "linux")]

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

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
fn commit_timestamp_snapshot_backlog_keeps_threads_bounded_and_attribution_working() {
    let repo = TestRepo::new_with_daemon_env(&[
        (
            "GIT_AI_TEST_COMMIT_FILE_TIMESTAMP_SNAPSHOT_DELAY_MS",
            "5000",
        ),
        ("GIT_AI_TEST_FORCE_COMMIT_FILE_TIMESTAMP_SNAPSHOT", "1"),
    ]);
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    let baseline_threads = daemon_thread_count(&repo);
    for index in 0..12 {
        let message = format!("Empty commit {index}");
        repo.git(&["commit", "--allow-empty", "-m", &message])
            .unwrap();
        file.assert_committed_lines(lines!["base".unattributed_human()]);
    }

    let current_threads = daemon_thread_count(&repo);
    assert!(
        current_threads <= baseline_threads + 4,
        "commit timestamp snapshot backlog grew daemon threads from {baseline_threads} to {current_threads}"
    );

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after snapshot pressure")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
