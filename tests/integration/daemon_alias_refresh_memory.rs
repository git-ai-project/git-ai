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
fn alias_refresh_burst_keeps_threads_bounded_and_attribution_working() {
    let repo = TestRepo::new_with_daemon_env(&[
        ("GIT_AI_TEST_ALIAS_CACHE_TTL_SECS", "0"),
        ("GIT_AI_TEST_ALIAS_REFRESH_DELAY_MS", "2000"),
    ]);
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    let baseline_threads = daemon_thread_count(&repo);
    let external = tempfile::tempdir().unwrap();
    let mut repo_paths = Vec::new();
    for index in 0..8 {
        let path = external.path().join(format!("repo-{index}"));
        fs::create_dir_all(&path).unwrap();
        let path_arg = path.to_string_lossy();
        repo.git_without_test_sync_for_test(&["-C", &path_arg, "init"], &[])
            .unwrap();
        repo.git_without_test_sync_for_test(
            &["-C", &path_arg, "config", "alias.ss", "status"],
            &[],
        )
        .unwrap();
        repo_paths.push(path);
    }

    for path in &repo_paths {
        let path_arg = path.to_string_lossy();
        repo.git_without_test_sync_for_test(&["-C", &path_arg, "ss"], &[])
            .unwrap();
    }
    for path in &repo_paths {
        let path_arg = path.to_string_lossy();
        repo.git_without_test_sync_for_test(&["-C", &path_arg, "ss"], &[])
            .unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(750));

    let current_threads = daemon_thread_count(&repo);
    assert!(
        current_threads <= baseline_threads + 4,
        "alias refresh burst grew daemon threads from {baseline_threads} to {current_threads}"
    );

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after alias refresh pressure")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
