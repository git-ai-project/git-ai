use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

const LARGE_FILE_COUNT: usize = 48;
const LARGE_LINE_BYTES: usize = 1_024 * 1_024;

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

fn assert_large_files_unattributed(repo: &TestRepo, expected_line: &str) {
    for index in 0..LARGE_FILE_COUNT {
        let mut file = repo.filename(&format!("large-{index}.txt"));
        file.assert_committed_lines(crate::lines![expected_line.unattributed_human()]);
    }
}

#[test]
fn repeated_large_blob_materialization_keeps_daemon_bounded_and_recovers() {
    let repo = TestRepo::new_dedicated_daemon();
    let large_line = "x".repeat(LARGE_LINE_BYTES);
    let large_contents = format!("{large_line}\n");
    for index in 0..LARGE_FILE_COUNT {
        fs::write(
            repo.path().join(format!("large-{index}.txt")),
            &large_contents,
        )
        .unwrap();
    }
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial repeated blob commit"])
        .unwrap();
    assert_large_files_unattributed(&repo, &large_line);

    // Ensure amend reconstruction has a working log while keeping setup below
    // the same aggregate content budget exercised by the amend itself.
    repo.git_ai(&["checkpoint", "human", "large-0.txt"])
        .unwrap();

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);
    #[cfg(target_os = "linux")]
    let baseline_threads = daemon_thread_count(&repo);

    // A root-commit amend considers every committed path. All files resolve to
    // one Git blob, which previously got cloned once per path in the daemon.
    repo.git_without_test_sync_for_test(&["commit", "--amend", "--no-edit", "--allow-empty"], &[])
        .unwrap();
    repo.sync_daemon();

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        eprintln!("repeated blob materialization HWM growth: {hwm_growth_kib} KiB");
        assert!(
            hwm_growth_kib < 40 * 1_024,
            "repeated blob materialization grew daemon HWM by {hwm_growth_kib} KiB"
        );
        let materialized_threads = daemon_thread_count(&repo);
        assert!(
            materialized_threads <= baseline_threads + 2,
            "batch reads must not retain worker threads: baseline={baseline_threads}, materialized={materialized_threads}"
        );
    }
    assert_large_files_unattributed(&repo, &large_line);

    let recovery_path = repo.path().join("recovery.txt");
    fs::write(&recovery_path, "AI recovery\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "recovery.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI commit after materialization pressure")
        .unwrap();
    assert_large_files_unattributed(&repo, &large_line);
    let mut recovery_file = repo.filename("recovery.txt");
    recovery_file.assert_committed_lines(crate::lines!["AI recovery".ai()]);
}
