use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs::{self, OpenOptions};
use std::io::Write;

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
fn oversized_historical_reflog_keeps_daemon_bounded_and_next_commit_attributed() {
    let mut repo = TestRepo::new_dedicated_daemon();
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.filename("tracked.txt")
        .assert_committed_lines(lines!["base".unattributed_human()]);

    let head_log = repo.path().join(".git/logs/HEAD");
    let existing = fs::read(&head_log).unwrap();
    let seed_line = existing
        .split_inclusive(|byte| *byte == b'\n')
        .next_back()
        .expect("initial commit should create a HEAD reflog row");
    assert!(seed_line.ends_with(b"\n"));
    let mut block = Vec::with_capacity(seed_line.len() * 1_024);
    for _ in 0..1_024 {
        block.extend_from_slice(seed_line);
    }
    let mut reflog = OpenOptions::new().append(true).open(&head_log).unwrap();
    while reflog.metadata().unwrap().len() < 64 * 1_024 * 1_024 {
        reflog.write_all(&block).unwrap();
    }
    reflog.flush().unwrap();
    drop(reflog);

    repo.restart_dedicated_daemon_for_test();

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);

    repo.git(&[
        "commit",
        "--allow-empty",
        "-m",
        "Seed cursor after oversized reflog",
    ])
    .unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        assert!(
            hwm_growth_kib < 32 * 1_024,
            "oversized historical reflog grew daemon HWM by {hwm_growth_kib} KiB"
        );
    }

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after oversized reflog")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}

#[test]
fn many_working_log_bases_keep_ref_cursor_and_attribution_working() {
    let mut repo = TestRepo::new_dedicated_daemon();
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);
    drop(file);

    let working_logs = repo.path().join(".git/ai/working_logs");
    for index in 1..=5_000_u64 {
        fs::create_dir_all(working_logs.join(format!("{index:040x}"))).unwrap();
    }
    repo.restart_dedicated_daemon_for_test();

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI commit after working-log pressure")
        .unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
