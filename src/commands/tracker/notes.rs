use std::process::Command;

pub fn is_already_reported(repo_path: &str, commit_sha: &str) -> bool {
    Command::new("git")
        .args(&[
            "-C",
            repo_path,
            "notes",
            "--ref=ai-tracker",
            "show",
            commit_sha,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn mark_reported(repo_path: &str, commit_sha: &str) -> Result<(), String> {
    let output = Command::new("git")
        .args(&[
            "-C",
            repo_path,
            "notes",
            "--ref=ai-tracker",
            "add",
            "-m",
            "reported",
            commit_sha,
        ])
        .output()
        .map_err(|e| e.to_string())?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}
