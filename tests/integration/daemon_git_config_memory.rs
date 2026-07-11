use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs::{self, File};
use std::io::{BufWriter, Write};

const INCLUDED_CONFIG_BYTES: usize = 4 * 1_024 * 1_024;

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

fn write_large_git_config(path: &std::path::Path) {
    let file = File::create(path).unwrap();
    let mut writer = BufWriter::new(file);
    let mut written = 0usize;
    let mut index = 0usize;
    while written < INCLUDED_CONFIG_BYTES {
        let section = format!(
            "[remote \"pressure-{index:08}\"]\n\turl = https://example.com/pressure/{index:08}.git\n"
        );
        writer.write_all(section.as_bytes()).unwrap();
        written += section.len();
        index += 1;
    }
    writer.flush().unwrap();
}

#[test]
fn oversized_included_git_config_keeps_daemon_bounded_and_attribution_working() {
    let repo = TestRepo::new_with_daemon_env(&[]);
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(crate::lines!["base".unattributed_human()]);

    let git_config_path = repo.path().join(".git/config");
    let original_config = fs::read(&git_config_path).unwrap();
    let included_config_path = repo.path().join(".git/pressure.config");
    write_large_git_config(&included_config_path);
    let mut config_with_include = original_config.clone();
    write!(
        config_with_include,
        "\n[include]\n\tpath = {}\n",
        included_config_path.display()
    )
    .unwrap();
    fs::write(&git_config_path, config_with_include).unwrap();
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
        eprintln!("included Git config HWM growth: {hwm_growth_kib} KiB");
        assert!(
            hwm_growth_kib < 32 * 1_024,
            "included Git config grew daemon HWM by {hwm_growth_kib} KiB"
        );
        assert!(
            daemon_thread_count(&repo) <= baseline_threads + 2,
            "Git config parsing must not retain worker threads"
        );
    }

    fs::write(&git_config_path, original_config).unwrap();
    fs::remove_file(included_config_path).unwrap();
    repo.stage_all_and_commit("AI edit after Git config pressure")
        .unwrap();
    file.assert_committed_lines(crate::lines!["base".unattributed_human(), "ai line".ai()]);
}
