#![cfg(unix)]

use crate::repos::test_repo::{DaemonTestScope, GitTestMode, TestRepo, real_git_executable};
use git_ai::daemon::DaemonConfig;
use git_ai::daemon::control_api::ControlRequest;
use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

fn latest_applied_seq(repo: &TestRepo) -> u64 {
    let response = git_ai::daemon::send_control_request_with_timeout(
        &repo.daemon_control_socket_path(),
        &ControlRequest::StatusFamily {
            repo_working_dir: repo.path().to_string_lossy().to_string(),
        },
        Duration::from_secs(2),
    )
    .expect("daemon status request should succeed");

    response
        .data
        .as_ref()
        .and_then(|data| data.get("latest_seq"))
        .and_then(serde_json::Value::as_u64)
        .expect("daemon status should include latest_seq")
}

fn wait_for_latest_seq_after(repo: &TestRepo, baseline: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        if latest_applied_seq(repo) > baseline {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }

    panic!("daemon did not ingest later git command while a prior trace connection stayed open");
}

fn run_git_tag_with_timeout(repo: &TestRepo, tag: &str) {
    let trace_target = DaemonConfig::trace2_event_target_for_path(&repo.daemon_trace_socket_path());
    let mut command = Command::new(real_git_executable());
    command
        .current_dir(repo.path())
        .args(["tag", tag])
        .env("GIT_TRACE2_EVENT", trace_target)
        .env("GIT_TRACE2_EVENT_NESTING", "10")
        .env("HOME", repo.test_home_path())
        .env(
            "GIT_CONFIG_GLOBAL",
            repo.test_home_path().join(".gitconfig"),
        )
        .env("XDG_CONFIG_HOME", repo.test_home_path().join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1");

    let mut child = command.spawn().expect("git tag should spawn");
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("git tag wait should work") {
            assert!(status.success(), "git tag exited with status {status}");
            return;
        }
        if start.elapsed() >= Duration::from_secs(3) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("git tag blocked while daemon trace listener had a held-open connection");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn test_held_open_trace_connection_does_not_starve_later_git_commands() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::Dedicated);
    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    let baseline = latest_applied_seq(&repo);

    let mut held_trace =
        UnixStream::connect(repo.daemon_trace_socket_path()).expect("connect trace socket");
    let held_start = serde_json::json!({
        "event": "start",
        "sid": "20260603T000000.000000-Pheldtrace",
        "argv": ["git", "status"],
        "cwd": repo.path(),
    });
    writeln!(held_trace, "{held_start}").expect("write held trace start");
    held_trace.flush().expect("flush held trace start");

    thread::sleep(Duration::from_millis(100));

    run_git_tag_with_timeout(&repo, "trace-listener-probe");
    wait_for_latest_seq_after(&repo, baseline);

    drop(held_trace);
}
