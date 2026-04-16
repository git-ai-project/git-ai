pub fn should_upload(repo_path: &str, commit_sha: &str, blacklist: &[String]) -> bool {
    let _ = (repo_path, commit_sha, blacklist);
    false
}
