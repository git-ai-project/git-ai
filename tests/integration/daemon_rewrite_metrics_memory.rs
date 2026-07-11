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
fn rewrite_metric_burst_keeps_threads_bounded_and_attribution_working() {
    let repo = TestRepo::new_with_daemon_env(&[
        ("GIT_AI_TEST_REWRITE_METRICS_DELAY_MS", "5000"),
        ("GIT_AI_TEST_FORCE_REWRITE_METRICS_ON_AMEND", "1"),
    ]);
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI commit").unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);

    let baseline_threads = daemon_thread_count(&repo);
    for _ in 0..8 {
        repo.git(&["commit", "--amend", "--no-edit"]).unwrap();
        file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
    }

    let current_threads = daemon_thread_count(&repo);
    assert!(
        current_threads <= baseline_threads + 4,
        "rewrite metric burst grew daemon threads from {baseline_threads} to {current_threads}"
    );

    fs::write(&file_path, "base\nai line\nsecond ai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after rewrite metric pressure")
        .unwrap();
    file.assert_committed_lines(lines![
        "base".unattributed_human(),
        "ai line".ai(),
        "second ai line".ai(),
    ]);
}
