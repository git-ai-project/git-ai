use super::config::TrackerConfig;

pub fn save_to_queue(repo_path: &str, commit_sha: &str, diff_gz: Vec<u8>) -> Result<(), String> {
    let _ = (repo_path, commit_sha, diff_gz);
    Ok(())
}

pub fn process_retries(config: &TrackerConfig) -> Result<(), String> {
    let _ = config;
    Ok(())
}
