use super::config::TrackerConfig;

pub fn upload_commit(
    repo_path: &str,
    commit_sha: &str,
    diff_gz: Vec<u8>,
    config: &TrackerConfig,
) -> Result<(), String> {
    let _ = (repo_path, commit_sha, diff_gz, config);
    Ok(())
}
