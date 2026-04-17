pub mod config;
pub mod diff;
pub mod filter;
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

    let current_refs = get_current_refs(repo_path);

    for (branch, old_sha) in pre_push_refs {
        let new_sha = match current_refs.get(branch) {
            Some(sha) if sha != old_sha => sha,
            _ => continue,
        };

        let commits = get_commits_in_range(repo_path, old_sha, new_sha);

        for commit_sha in commits {
            if !filter::should_upload(repo_path, &commit_sha, &config.blacklist) {
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
                }
                Err(e) => {
                    tracing::debug!("tracker upload failed {}: {}", &commit_sha, e);
                    let _ = retry::save_to_queue(repo_path, &commit_sha, diff_gz, remote, branch);
                }
            }
        }
    }
}

fn get_current_refs(repo_path: &str) -> HashMap<String, String> {
    let output = match Command::new("git")
        .args([
            "-C",
            repo_path,
            "for-each-ref",
            "refs/heads/",
            "--format=%(refname:short) %(objectname)",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return HashMap::new(),
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let mut refs = HashMap::new();

    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() == 2 {
            refs.insert(parts[0].to_string(), parts[1].to_string());
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
