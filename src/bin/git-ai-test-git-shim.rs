use git_ai::daemon::test_sync::{
    tracked_parsed_git_invocation_for_test_sync, tracks_parsed_git_invocation_for_test_sync,
};
use serde::Serialize;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
#[cfg(not(unix))]
use std::process::Stdio;

#[derive(Serialize)]
struct StartedGitInvocationLogEntry {
    command: Option<String>,
    command_args: Vec<String>,
    cwd: Option<String>,
}

fn append_started_log(log_path: &PathBuf, argv: &[String]) -> Result<(), String> {
    let cwd = env::current_dir().map_err(|e| format!("read shim cwd failed: {e}"))?;
    let parsed = tracked_parsed_git_invocation_for_test_sync(argv, &cwd);
    if !tracks_parsed_git_invocation_for_test_sync(&parsed) {
        return Ok(());
    }

    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create shim log dir failed: {e}"))?;
    }

    let entry = StartedGitInvocationLogEntry {
        command: parsed.command.clone(),
        command_args: parsed.command_args.clone(),
        cwd: Some(cwd.to_string_lossy().to_string()),
    };
    let mut line = serde_json::to_vec(&entry).map_err(|e| format!("serialize shim log: {e}"))?;
    line.push(b'\n');

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| format!("open shim log failed: {e}"))?;
    file.write_all(&line)
        .map_err(|e| format!("write shim log failed: {e}"))?;
    file.flush()
        .map_err(|e| format!("flush shim log failed: {e}"))?;
    Ok(())
}

#[cfg(unix)]
fn exec_target(target: &str, argv: &[String]) -> ! {
    let error = Command::new(target).args(argv).exec();
    eprintln!("git-ai-test-git-shim failed to exec {target}: {error}");
    std::process::exit(127);
}

#[cfg(not(unix))]
fn exec_target(target: &str, argv: &[String]) -> ! {
    match Command::new(target)
        .args(argv)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
    {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("git-ai-test-git-shim failed to spawn {target}: {error}");
            std::process::exit(127);
        }
    }
}

#[cfg(unix)]
fn main() {
    let target = env::var("GIT_AI_TEST_GIT_SHIM_TARGET")
        .unwrap_or_else(|_| panic!("GIT_AI_TEST_GIT_SHIM_TARGET is required"));
    let argv = env::args().skip(1).collect::<Vec<_>>();
    if let Ok(log_path) = env::var("GIT_AI_TEST_SYNC_START_LOG") {
        let log_path = PathBuf::from(log_path);
        if let Err(error) = append_started_log(&log_path, &argv) {
            panic!("git-ai-test-git-shim failed: {error}");
        }
    }
    exec_target(&target, &argv);
}

#[cfg(not(unix))]
fn main() {
    let target = env::var("GIT_AI_TEST_GIT_SHIM_TARGET")
        .unwrap_or_else(|_| panic!("GIT_AI_TEST_GIT_SHIM_TARGET is required"));
    let argv = env::args().skip(1).collect::<Vec<_>>();
    if let Ok(log_path) = env::var("GIT_AI_TEST_SYNC_START_LOG") {
        let log_path = PathBuf::from(log_path);
        if let Err(error) = append_started_log(&log_path, &argv) {
            panic!("git-ai-test-git-shim failed: {error}");
        }
    }
    exec_target(&target, &argv)
}
