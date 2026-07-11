use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::daemon::open_local_socket_stream_with_timeout;
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::time::Duration;

const CONTROL_FILE_BYTES: u64 = 64 * 1_024 * 1_024;

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
fn oversized_gitdir_control_file_keeps_daemon_bounded_and_attribution_working() {
    let repo = TestRepo::new_dedicated_daemon();
    let tracked_path = repo.path().join("tracked.txt");
    fs::write(&tracked_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut tracked = repo.filename("tracked.txt");
    tracked.assert_committed_lines(crate::lines!["base".unattributed_human()]);

    let fake_git_dir = repo.path().join(".git/ai/control-pressure");
    fs::create_dir_all(&fake_git_dir).unwrap();
    let gitdir_path = fake_git_dir.join("gitdir");
    let mut gitdir = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&gitdir_path)
        .unwrap();
    gitdir.write_all(b"/tmp/target/.git\n").unwrap();
    gitdir
        .seek(SeekFrom::Start(CONTROL_FILE_BYTES - 1))
        .unwrap();
    gitdir.write_all(&[0]).unwrap();
    gitdir.flush().unwrap();

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);

    let sid = "oversized-control-metadata";
    let frames = [
        serde_json::json!({
            "event": "start",
            "sid": sid,
            "argv": ["git", "status"],
            "cwd": repo.path(),
            "time_ns": 1,
        }),
        serde_json::json!({
            "event": "def_repo",
            "sid": sid,
            "repo": fake_git_dir,
            "time_ns": 2,
        }),
        serde_json::json!({
            "event": "atexit",
            "sid": sid,
            "code": 0,
            "time_ns": 3,
        }),
    ];
    let mut stream = open_local_socket_stream_with_timeout(
        &repo.daemon_trace_socket_path(),
        Duration::from_secs(2),
    )
    .expect("connect trace socket");
    for frame in frames {
        writeln!(stream, "{frame}").unwrap();
    }
    stream.flush().unwrap();
    drop(stream);
    std::thread::sleep(Duration::from_millis(500));

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        eprintln!("Git control metadata HWM growth: {hwm_growth_kib} KiB");
        assert!(
            hwm_growth_kib < 24 * 1_024,
            "oversized Git control metadata grew daemon HWM by {hwm_growth_kib} KiB"
        );
    }

    fs::remove_dir_all(fake_git_dir).unwrap();
    fs::write(&tracked_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after control metadata pressure")
        .unwrap();
    tracked.assert_committed_lines(crate::lines!["base".unattributed_human(), "ai line".ai()]);
}
