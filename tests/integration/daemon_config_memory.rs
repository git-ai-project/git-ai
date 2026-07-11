use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs::{self, File};
use std::io::{BufWriter, Write};

const CONFIG_FILE_BYTES: usize = 4 * 1_024 * 1_024;

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

fn write_large_config(path: &std::path::Path) {
    let file = File::create(path).unwrap();
    let mut writer = BufWriter::new(file);
    writer
        .write_all(b"{\"exclude_prompts_in_repositories\":[")
        .unwrap();
    let mut written = 0usize;
    let mut index = 0usize;
    while written < CONFIG_FILE_BYTES {
        if index > 0 {
            writer.write_all(b",").unwrap();
            written += 1;
        }
        let pattern = format!("\"repository-{index:08}/**\"");
        writer.write_all(pattern.as_bytes()).unwrap();
        written += pattern.len();
        index += 1;
    }
    writer.write_all(b"]}").unwrap();
    writer.flush().unwrap();
}

#[test]
fn oversized_config_keeps_daemon_bounded_and_attribution_working() {
    let repo = TestRepo::new_with_daemon_env(&[]);
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(crate::lines!["base".unattributed_human()]);

    let config_path = repo.test_home_path().join(".git-ai/config.json");
    let original_config = fs::read(&config_path).unwrap_or_else(|_| b"{}".to_vec());
    write_large_config(&config_path);
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
        eprintln!("config input HWM growth: {hwm_growth_kib} KiB");
        assert!(
            hwm_growth_kib < 32 * 1_024,
            "oversized config grew daemon HWM by {hwm_growth_kib} KiB"
        );
        assert!(
            daemon_thread_count(&repo) <= baseline_threads + 2,
            "config parsing must not retain worker threads"
        );
    }

    fs::write(config_path, original_config).unwrap();
    repo.stage_all_and_commit("AI edit after config pressure")
        .unwrap();
    file.assert_committed_lines(crate::lines!["base".unattributed_human(), "ai line".ai()]);
}
