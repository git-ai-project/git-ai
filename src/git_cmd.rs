use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::core::git_binary::git_path;

/// Create a `git -C <repo_path>` command with `GIT_TRACE2_EVENT=0` set
/// to prevent recursive daemon events. Caller can further configure
/// stdout/stderr/stdin before spawning.
pub fn git_command(repo_path: &Path) -> Command {
    let mut cmd = Command::new(git_path());
    cmd.arg("-C")
        .arg(repo_path)
        .env("GIT_TRACE2_EVENT", "0");
    cmd
}

/// Run a git command in the given repo, returning trimmed stdout on success.
pub fn git_in_repo(repo_path: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_command(repo_path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("git failed to execute: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git {} failed: {}", args.join(" "), stderr))
    }
}

/// Run a git command with data piped to stdin, returning raw stdout bytes.
pub fn git_in_repo_stdin(repo_path: &Path, args: &[&str], stdin_data: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = git_command(repo_path)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git failed to spawn: {}", e))?;

    if let Some(ref mut stdin_pipe) = child.stdin {
        stdin_pipe
            .write_all(stdin_data)
            .map_err(|e| format!("failed to write to git stdin: {}", e))?;
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .map_err(|e| format!("git failed to complete: {}", e))?;

    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git {} failed: {}", args.join(" "), stderr))
    }
}
