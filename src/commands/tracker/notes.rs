pub fn is_already_reported(repo_path: &str, commit_sha: &str) -> bool {
    let _ = (repo_path, commit_sha);
    false
}

pub fn mark_reported(repo_path: &str, commit_sha: &str) -> Result<(), String> {
    let _ = (repo_path, commit_sha);
    Ok(())
}
