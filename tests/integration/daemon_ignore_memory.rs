use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs::{self, File};
use std::io::{BufWriter, Write};

const IGNORE_FILE_BYTES: usize = 4 * 1_024 * 1_024;

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

fn write_large_ignore_file(repo: &TestRepo) {
    let file = File::create(repo.path().join(".git-ai-ignore")).unwrap();
    let mut writer = BufWriter::new(file);
    let mut written = 0usize;
    let mut index = 0usize;
    while written < IGNORE_FILE_BYTES {
        let line = format!("generated-{index:08}.txt\n");
        writer.write_all(line.as_bytes()).unwrap();
        written += line.len();
        index += 1;
    }
    writer.flush().unwrap();
}

#[test]
fn oversized_ignore_configuration_keeps_daemon_bounded_and_attribution_working() {
    let repo = TestRepo::new_dedicated_daemon();
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(crate::lines!["base".unattributed_human()]);

    write_large_ignore_file(&repo);
    fs::write(&file_path, "base\nai line\n").unwrap();

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);
    #[cfg(target_os = "linux")]
    let baseline_threads = daemon_thread_count(&repo);

    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        eprintln!("ignore configuration HWM growth: {hwm_growth_kib} KiB");
        assert!(
            hwm_growth_kib < 32 * 1_024,
            "ignore configuration grew daemon HWM by {hwm_growth_kib} KiB"
        );
        let checkpoint_threads = daemon_thread_count(&repo);
        assert!(
            checkpoint_threads <= baseline_threads + 2,
            "ignore parsing must not retain worker threads: baseline={baseline_threads}, checkpoint={checkpoint_threads}"
        );
    }

    fs::remove_file(repo.path().join(".git-ai-ignore")).unwrap();
    repo.stage_all_and_commit("AI edit after ignore pressure")
        .unwrap();
    file.assert_committed_lines(crate::lines!["base".unattributed_human(), "ai line".ai()]);
}
