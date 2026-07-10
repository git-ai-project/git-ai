use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

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
fn family_actor_churn_keeps_daemon_bounded_and_attribution_working() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_MAX_FAMILY_ACTORS", "4")]);
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);

    let external = tempfile::tempdir().unwrap();
    for index in 0..12 {
        let path = external.path().join(format!("repo-{index}"));
        fs::create_dir_all(&path).unwrap();
        let path = path.to_string_lossy();
        repo.git_without_test_sync_for_test(&["-C", &path, "init"], &[])
            .unwrap();
        repo.git_without_test_sync_for_test(
            &["-C", &path, "checkout", "--orphan", "actor-pressure"],
            &[],
        )
        .unwrap();
    }
    repo.sync_daemon();

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        assert!(
            hwm_growth_kib < 24 * 1024,
            "family actor churn grew daemon HWM by {hwm_growth_kib} KiB"
        );
    }

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after family churn")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
