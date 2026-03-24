#[path = "integration/repos/mod.rs"]
mod repos;

use git_ai::daemon::{
    ControlRequest, DaemonConfig, local_socket_connects_with_timeout, send_control_request,
};
use repos::test_repo::{GitTestMode, TestRepo, get_binary_path, real_git_executable};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::Duration;

const DAEMON_TEST_PROBE_TIMEOUT: Duration = Duration::from_millis(100);

fn git_common_dir(repo: &TestRepo) -> PathBuf {
    let common_dir = repo
        .git(&["rev-parse", "--git-common-dir"])
        .expect("rev-parse --git-common-dir should succeed");
    let common_dir = PathBuf::from(common_dir.trim());
    if common_dir.is_absolute() {
        common_dir
    } else {
        repo.path().join(common_dir)
    }
}

fn read_global_git_config(repo: &TestRepo, key: &str) -> Option<String> {
    let mut command = Command::new(real_git_executable());
    command.args(["config", "--global", "--get", key]);
    command.current_dir(repo.path());
    configure_test_home_env(&mut command, repo);
    let output = command.output().expect("failed to read global git config");

    if output.status.success() {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if value.is_empty() { None } else { Some(value) }
    } else {
        None
    }
}

fn daemon_trace_socket_path(repo: &TestRepo) -> PathBuf {
    repo.daemon_trace_socket_path()
}

fn daemon_control_socket_path(repo: &TestRepo) -> PathBuf {
    repo.daemon_control_socket_path()
}

fn configure_test_home_env(command: &mut Command, repo: &TestRepo) {
    command.env("HOME", repo.test_home_path());
    command.env(
        "GIT_CONFIG_GLOBAL",
        repo.test_home_path().join(".gitconfig"),
    );
    #[cfg(windows)]
    {
        command.env("USERPROFILE", repo.test_home_path());
        command.env(
            "APPDATA",
            repo.test_home_path().join("AppData").join("Roaming"),
        );
        command.env(
            "LOCALAPPDATA",
            repo.test_home_path().join("AppData").join("Local"),
        );
    }
}

fn configure_test_daemon_env(command: &mut Command, repo: &TestRepo) {
    command.env("GIT_AI_DAEMON_HOME", repo.daemon_home_path());
    command.env(
        "GIT_AI_DAEMON_CONTROL_SOCKET",
        daemon_control_socket_path(repo),
    );
    command.env("GIT_AI_DAEMON_TRACE_SOCKET", daemon_trace_socket_path(repo));
}

fn write_async_mode_config(repo: &TestRepo) {
    let config_dir = repo.test_home_path().join(".git-ai");
    fs::create_dir_all(&config_dir).expect("failed to create async mode config dir");
    let config_path = config_dir.join("config.json");
    let config = serde_json::json!({
        "git_path": real_git_executable(),
        "disable_auto_updates": true,
        "feature_flags": {
            "async_mode": true,
            "git_hooks_enabled": false
        },
        "quiet": false
    });
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("failed to serialize async mode config"),
    )
    .expect("failed to write async mode config");
}

fn git_ai_with_async_daemon_env(repo: &TestRepo, args: &[&str]) -> Result<String, String> {
    let daemon_home = repo.daemon_home_path().to_string_lossy().to_string();
    let control_socket = daemon_control_socket_path(repo)
        .to_string_lossy()
        .to_string();
    let trace_socket = daemon_trace_socket_path(repo).to_string_lossy().to_string();
    let envs = [
        ("GIT_AI_ASYNC_MODE", "true"),
        ("GIT_AI_DAEMON_HOME", daemon_home.as_str()),
        ("GIT_AI_DAEMON_CONTROL_SOCKET", control_socket.as_str()),
        ("GIT_AI_DAEMON_TRACE_SOCKET", trace_socket.as_str()),
    ];
    repo.git_ai_with_env(args, &envs)
}

fn wait_for_daemon_sockets(repo: &TestRepo) {
    let control = daemon_control_socket_path(repo);
    let trace = daemon_trace_socket_path(repo);
    for _ in 0..200 {
        let status = send_control_request(
            &control,
            &ControlRequest::StatusFamily {
                repo_working_dir: repo.canonical_path().to_string_lossy().to_string(),
            },
        );
        if status.is_ok()
            && local_socket_connects_with_timeout(&trace, DAEMON_TEST_PROBE_TIMEOUT).is_ok()
        {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "daemon sockets did not become ready: control={}, trace={}",
        control.display(),
        trace.display()
    );
}

fn wait_for_daemon_latest_seq(repo: &TestRepo, min_seq: u64) {
    let control = daemon_control_socket_path(repo);
    let repo_working_dir = repo.canonical_path().to_string_lossy().to_string();
    for _ in 0..200 {
        let response = send_control_request(
            &control,
            &ControlRequest::StatusFamily {
                repo_working_dir: repo_working_dir.clone(),
            },
        )
        .expect("status request should succeed while waiting for traced command");
        let latest_seq = response
            .data
            .as_ref()
            .and_then(|data| data.get("latest_seq"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if latest_seq >= min_seq {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "daemon did not observe traced command for {}",
        repo.canonical_path().display()
    );
}

fn daemon_command_output(repo: &TestRepo, args: &[&str], cwd: &Path) -> Output {
    let mut command = Command::new(get_binary_path());
    command.args(args).current_dir(cwd);
    configure_test_home_env(&mut command, repo);
    configure_test_daemon_env(&mut command, repo);
    command
        .output()
        .expect("failed to invoke git-ai daemon command")
}

fn daemon_status_response(home_repo: &TestRepo, target_repo: &TestRepo) -> Value {
    let output = daemon_command_output(
        home_repo,
        &[
            "daemon",
            "status",
            "--repo",
            target_repo.canonical_path().to_string_lossy().as_ref(),
        ],
        target_repo.path(),
    );
    assert!(
        output.status.success(),
        "daemon status command should succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("daemon status output should be valid JSON")
}

fn assert_daemon_status_ok_after_launch_repo_removed(home_repo: &TestRepo, target_repo: &TestRepo) {
    let response = daemon_status_response(home_repo, target_repo);
    assert!(
        response.get("ok").and_then(Value::as_bool) == Some(true),
        "daemon status should remain ok after deleting launch repo cwd: {}",
        response
    );
}

fn shutdown_daemon(home_repo: &TestRepo) {
    let output = daemon_command_output(
        home_repo,
        &["daemon", "shutdown"],
        home_repo.test_home_path(),
    );
    assert!(
        output.status.success(),
        "daemon shutdown command should succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_for_child_exit(child: &mut Child) {
    for _ in 0..100 {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn async_mode_wrapper_commit_passthrough_skips_git_ai_side_effects() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let ai_dir = git_common_dir(&repo).join("ai");
    let _ = fs::remove_dir_all(&ai_dir);
    assert!(
        ai_dir.symlink_metadata().is_err(),
        "expected test setup to start without .git/ai state"
    );

    fs::write(repo.path().join("async-mode.txt"), "async mode test\n")
        .expect("failed to write test file");

    repo.git_with_env(
        &["add", "async-mode.txt"],
        &[("GIT_AI_ASYNC_MODE", "true")],
        None,
    )
    .expect("git add should succeed in async mode");
    repo.git_with_env(
        &["commit", "-m", "async passthrough commit"],
        &[("GIT_AI_ASYNC_MODE", "true")],
        None,
    )
    .expect("git commit should succeed in async mode");

    assert!(
        ai_dir.symlink_metadata().is_err(),
        "async mode wrapper should passthrough without creating .git/ai side effects"
    );
}

#[test]
fn install_hooks_async_mode_sets_daemon_trace2_global_config() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    let output = git_ai_with_async_daemon_env(&repo, &["install-hooks", "--dry-run=false"])
        .expect("install-hooks should succeed in async mode");

    assert!(
        !output.contains("trace2.eventTarget") && !output.contains("trace2.eventNesting"),
        "async preflight should run silently without trace2 config output"
    );

    let expected_trace_socket = daemon_trace_socket_path(&repo);
    let expected_target = DaemonConfig::trace2_event_target_for_path(&expected_trace_socket);

    let target = read_global_git_config(&repo, "trace2.eventTarget");
    let nesting = read_global_git_config(&repo, "trace2.eventNesting");

    assert_eq!(target.as_deref(), Some(expected_target.as_str()));
    assert_eq!(nesting.as_deref(), Some("10"));
}

#[test]
fn install_hooks_async_mode_dry_run_does_not_write_trace2_global_config() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    git_ai_with_async_daemon_env(&repo, &["install-hooks", "--dry-run=true"])
        .expect("install-hooks dry-run should succeed in async mode");

    let target = read_global_git_config(&repo, "trace2.eventTarget");
    let nesting = read_global_git_config(&repo, "trace2.eventNesting");

    assert!(
        target.is_none(),
        "install-hooks dry-run should not set trace2.eventTarget"
    );
    assert!(
        nesting.is_none(),
        "install-hooks dry-run should not set trace2.eventNesting"
    );
}

#[test]
fn install_hooks_async_mode_trace2_target_routes_real_git_trace_to_daemon() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    git_ai_with_async_daemon_env(&repo, &["install-hooks", "--dry-run=false"])
        .expect("install-hooks should succeed in async mode");

    let start_output = daemon_command_output(&repo, &["daemon", "start"], repo.path());
    assert!(
        start_output.status.success(),
        "daemon start should succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&start_output.stdout),
        String::from_utf8_lossy(&start_output.stderr)
    );
    wait_for_daemon_sockets(&repo);

    let mut git_command = Command::new(real_git_executable());
    git_command.args(["status", "--short"]);
    git_command.current_dir(repo.path());
    configure_test_home_env(&mut git_command, &repo);
    let git_output = git_command
        .output()
        .expect("failed to run traced git status");
    assert!(
        git_output.status.success(),
        "traced git status should succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&git_output.stdout),
        String::from_utf8_lossy(&git_output.stderr)
    );

    wait_for_daemon_latest_seq(&repo, 1);
    shutdown_daemon(&repo);
}

#[test]
fn async_mode_checkpoint_starts_daemon_when_down() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    write_async_mode_config(&repo);

    let control = daemon_control_socket_path(&repo);
    let trace = daemon_trace_socket_path(&repo);
    let _ = fs::remove_file(&control);
    let _ = fs::remove_file(&trace);

    fs::write(
        repo.path().join("async-checkpoint.txt"),
        "async checkpoint\n",
    )
    .expect("failed to write async checkpoint file");

    let output = repo
        .git_ai(&["checkpoint", "mock_ai", "async-checkpoint.txt"])
        .expect("async mode checkpoint should succeed");

    assert!(
        !output.contains("[BENCHMARK] Starting checkpoint run"),
        "async mode checkpoint should delegate to daemon instead of running synchronously: {}",
        output
    );

    wait_for_daemon_sockets(&repo);
    assert_daemon_status_ok_after_launch_repo_removed(&repo, &repo);
    shutdown_daemon(&repo);
}

#[test]
fn daemon_status_does_not_self_emit_trace2_events() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    fs::create_dir_all(repo.test_home_path()).expect("failed to create test HOME directory");
    let trace_target = DaemonConfig::trace2_event_target_for_path(&daemon_trace_socket_path(&repo));

    let mut set_target_command = Command::new(real_git_executable());
    set_target_command.args(["config", "--global", "trace2.eventTarget", &trace_target]);
    set_target_command.current_dir(repo.path());
    configure_test_home_env(&mut set_target_command, &repo);
    let set_target = set_target_command
        .output()
        .expect("failed to set global trace2.eventTarget");
    assert!(
        set_target.status.success(),
        "setting trace2.eventTarget failed: stdout={} stderr={}",
        String::from_utf8_lossy(&set_target.stdout),
        String::from_utf8_lossy(&set_target.stderr)
    );

    let mut set_nesting_command = Command::new(real_git_executable());
    set_nesting_command.args(["config", "--global", "trace2.eventNesting", "10"]);
    set_nesting_command.current_dir(repo.path());
    configure_test_home_env(&mut set_nesting_command, &repo);
    let set_nesting = set_nesting_command
        .output()
        .expect("failed to set global trace2.eventNesting");
    assert!(
        set_nesting.status.success(),
        "setting trace2.eventNesting failed: stdout={} stderr={}",
        String::from_utf8_lossy(&set_nesting.stdout),
        String::from_utf8_lossy(&set_nesting.stderr)
    );

    let mut daemon_cmd = Command::new(repos::test_repo::get_binary_path());
    daemon_cmd
        .arg("daemon")
        .arg("run")
        .current_dir(repo.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_test_home_env(&mut daemon_cmd, &repo);
    configure_test_daemon_env(&mut daemon_cmd, &repo);
    let mut daemon = daemon_cmd.spawn().expect("failed to start daemon");

    wait_for_daemon_sockets(&repo);

    let repo_working_dir = repo.canonical_path().to_string_lossy().to_string();
    let status_response = send_control_request(
        &daemon_control_socket_path(&repo),
        &ControlRequest::StatusFamily { repo_working_dir },
    )
    .expect("status request failed");
    assert!(status_response.ok, "daemon status should succeed");
    let status_data = status_response.data.expect("status response missing data");
    let latest_seq = status_data
        .get("latest_seq")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(u64::MAX);

    assert_eq!(
        latest_seq, 0,
        "daemon status should not create self-trace events when global trace2 target points to daemon"
    );

    let _ = send_control_request(
        &daemon_control_socket_path(&repo),
        &ControlRequest::Shutdown,
    );
    for _ in 0..100 {
        match daemon.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
fn daemon_run_survives_deleted_launch_repo_cwd() {
    let launch_repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let target_repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    let mut daemon_cmd = Command::new(get_binary_path());
    daemon_cmd
        .arg("daemon")
        .arg("run")
        .current_dir(launch_repo.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_test_home_env(&mut daemon_cmd, &launch_repo);
    configure_test_daemon_env(&mut daemon_cmd, &launch_repo);
    let mut daemon = daemon_cmd.spawn().expect("failed to start daemon");

    wait_for_daemon_sockets(&launch_repo);
    fs::remove_dir_all(launch_repo.path()).expect("failed to remove launch repo");

    assert_daemon_status_ok_after_launch_repo_removed(&launch_repo, &target_repo);

    shutdown_daemon(&launch_repo);
    wait_for_child_exit(&mut daemon);
}

#[test]
fn daemon_start_survives_deleted_launch_repo_cwd() {
    let launch_repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let target_repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    let output = daemon_command_output(&launch_repo, &["daemon", "start"], launch_repo.path());
    assert!(
        output.status.success(),
        "daemon start should succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    wait_for_daemon_sockets(&launch_repo);
    fs::remove_dir_all(launch_repo.path()).expect("failed to remove launch repo");

    assert_daemon_status_ok_after_launch_repo_removed(&launch_repo, &target_repo);

    shutdown_daemon(&launch_repo);
}
