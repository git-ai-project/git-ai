use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

const TRACE2_DISABLED_ENV: [(&str, &str); 3] = [
    ("GIT_TRACE2", "0"),
    ("GIT_TRACE2_EVENT", "0"),
    ("GIT_TRACE2_PERF", "0"),
];

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

#[test]
fn oversized_loose_commit_keeps_daemon_bounded_and_attribution_working() {
    let mut repo = TestRepo::new_dedicated_daemon();
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.filename("tracked.txt")
        .assert_committed_lines(lines!["base".unattributed_human()]);

    let message_path = repo.test_home_path().join("oversized-commit-message.txt");
    fs::write(&message_path, vec![b'x'; 64 * 1_024 * 1_024]).unwrap();
    repo.git_og_with_env(
        &[
            "commit",
            "--allow-empty",
            "-F",
            message_path.to_str().unwrap(),
        ],
        &TRACE2_DISABLED_ENV,
    )
    .unwrap();
    repo.filename("tracked.txt")
        .assert_committed_lines(lines!["base".unattributed_human()]);

    repo.restart_dedicated_daemon_for_test();

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);

    repo.git_ai(&["checkpoint", "human", "tracked.txt"])
        .unwrap();

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        assert!(
            hwm_growth_kib < 24 * 1_024,
            "oversized loose commit grew daemon HWM by {hwm_growth_kib} KiB"
        );
    }

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after loose object pressure")
        .unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
