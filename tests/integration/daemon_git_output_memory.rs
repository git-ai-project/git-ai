#![cfg(unix)]

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{TestRepo, real_git_executable};
use std::fs;
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

// The reader retains 32 MiB; leave room for Vec growth and its two bounded reader stacks.
#[cfg(target_os = "linux")]
const MAX_DAEMON_HWM_GROWTH_KIB: u64 = 48 * 1_024;

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
fn wait_for_internal_git_threads_to_exit(repo: &TestRepo) -> usize {
    let pid = repo.daemon_pid().expect("test repo should own a daemon");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let active = fs::read_dir(format!("/proc/{pid}/task"))
            .unwrap()
            .flatten()
            .filter(|entry| {
                fs::read_to_string(entry.path().join("comm"))
                    .is_ok_and(|name| name.trim().starts_with("git-ai-git-"))
            })
            .count();
        if active == 0 || Instant::now() >= deadline {
            return active;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn oversized_internal_git_stdout_keeps_daemon_bounded_and_attribution_working() {
    let wrapper_dir = tempfile::tempdir().unwrap();
    let wrapper_path = wrapper_dir.path().join("pressure-git");
    let pressure_flag = wrapper_dir.path().join("emit-oversized-output");
    let pressure_marker = wrapper_dir.path().join("oversized-output-emitted");
    fs::write(
        &wrapper_path,
        format!(
            "#!/bin/sh\n\
             if [ -f '{}' ]; then\n\
               case \"$*\" in\n\
                 *--abbrev-ref*)\n\
                   printf hit > '{}'\n\
                   dd if=/dev/zero bs=1048576 count=64 2>/dev/null | tr '\\000' x\n\
                   printf '\\n'\n\
                   exit 0\n\
                   ;;\n\
               esac\n\
             fi\n\
             exec '{}' \"$@\"\n",
            pressure_flag.display(),
            pressure_marker.display(),
            real_git_executable(),
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&wrapper_path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper_path, permissions).unwrap();

    let wrapper = wrapper_path.to_string_lossy().to_string();
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_GIT_PATH", wrapper.as_str())]);
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);
    #[cfg(target_os = "linux")]
    assert_eq!(wait_for_internal_git_threads_to_exit(&repo), 0);

    fs::write(&pressure_flag, b"enabled").unwrap();
    fs::write(&file_path, "base\nuntracked line\n").unwrap();
    repo.git_ai(&["checkpoint", "human", "tracked.txt"])
        .unwrap();
    repo.sync_daemon_force();
    assert!(
        pressure_marker.exists(),
        "daemon should execute the oversized-output Git wrapper branch"
    );
    fs::remove_file(&pressure_flag).unwrap();

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        assert!(
            hwm_growth_kib < MAX_DAEMON_HWM_GROWTH_KIB,
            "oversized internal Git stdout grew daemon HWM by {hwm_growth_kib} KiB"
        );
        assert_eq!(
            wait_for_internal_git_threads_to_exit(&repo),
            0,
            "internal Git I/O threads must exit after command completion"
        );
    }

    fs::write(&file_path, "base\nknown human line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "tracked.txt"])
        .unwrap();
    fs::write(&file_path, "base\nknown human line\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after oversized Git output")
        .unwrap();
    file.assert_committed_lines(lines![
        "base".unattributed_human(),
        "known human line".human(),
        "ai line".ai(),
    ]);
}
