pub mod config;
pub mod diff;
pub mod filter;
pub mod log;
pub mod notes;
pub mod retry;
pub mod upload;

use std::collections::HashMap;
use std::process::Command;

pub fn report_pushed_commits(
    repo_path: &str,
    pre_push_refs: &HashMap<String, String>,
    remote: &str,
) {
    let config = match config::load_config() {
        Some(c) => c,
        None => return,
    };

    tracing::debug!("tracker: processing push to remote {}", remote);

    let repo_url = get_remote_url(repo_path, remote).unwrap_or_default();
    let post_push_refs = get_remote_refs(repo_path, remote);

    for (branch, old_sha) in pre_push_refs {
        let new_sha = match post_push_refs.get(branch) {
            Some(sha) if sha != old_sha => sha,
            _ => continue,
        };

        let commits = get_commits_in_range(repo_path, old_sha, new_sha);

        for commit_sha in commits {
            if let Err(reason) =
                filter::should_upload(repo_path, &repo_url, &commit_sha, &config.blacklist)
            {
                log::append_log(
                    log::LogStatus::Skipped,
                    &commit_sha,
                    remote,
                    branch,
                    repo_path,
                    Some(&reason),
                );
                continue;
            }

            let diff_gz = match diff::collect_code_diff(repo_path, &commit_sha) {
                Ok(gz) => gz,
                Err(e) => {
                    tracing::debug!("tracker diff failed {}: {}", &commit_sha, e);
                    continue;
                }
            };

            match upload::upload_commit(
                repo_path,
                &commit_sha,
                diff_gz.clone(),
                &config,
                remote,
                branch,
            ) {
                Ok(()) => {
                    let _ = notes::mark_reported(repo_path, &commit_sha);
                    log::append_log(
                        log::LogStatus::Uploaded,
                        &commit_sha,
                        remote,
                        branch,
                        repo_path,
                        None,
                    );
                }
                Err(e) => {
                    tracing::debug!("tracker upload failed {}: {}", &commit_sha, e);
                    log::append_log(
                        log::LogStatus::Failed,
                        &commit_sha,
                        remote,
                        branch,
                        repo_path,
                        Some(&e),
                    );
                    let _ = retry::save_to_queue(repo_path, &commit_sha, diff_gz, remote, branch);
                }
            }
        }
    }
}

fn get_remote_url(repo_path: &str, remote: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", repo_path, "remote", "get-url", remote])
        .output()
        .ok()?;
    if output.status.success() {
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !url.is_empty() {
            return Some(url);
        }
    }
    None
}

fn get_remote_refs(repo_path: &str, remote: &str) -> HashMap<String, String> {
    let output = match Command::new("git")
        .args(["-C", repo_path, "ls-remote", "--heads", remote])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return HashMap::new(),
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let mut refs = HashMap::new();

    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        if parts.len() == 2 {
            let sha = parts[0];
            let refname = parts[1].trim_start_matches("refs/heads/");
            refs.insert(refname.to_string(), sha.to_string());
        }
    }

    refs
}

fn get_commits_in_range(repo_path: &str, old_sha: &str, new_sha: &str) -> Vec<String> {
    let output = match Command::new("git")
        .args([
            "-C",
            repo_path,
            "rev-list",
            "--ancestry-path",
            &format!("{}..{}", old_sha, new_sha),
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().map(|s| s.to_string()).collect()
}
