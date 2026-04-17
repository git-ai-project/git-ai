use crate::commands::tracker::notes;
use serde::Deserialize;
use std::process::Command;

const MANUAL_ADDED_LINES_THRESHOLD: i32 = 300;

pub fn should_upload(repo_path: &str, commit_sha: &str, blacklist: &[String]) -> bool {
    let sha7 = &commit_sha[..commit_sha.len().min(7)];

    if is_blacklisted(repo_path, blacklist) {
        tracing::debug!("tracker filter: {} skipped (blacklisted)", sha7);
        return false;
    }
    if notes::is_already_reported(repo_path, commit_sha) {
        tracing::debug!("tracker filter: {} skipped (already reported)", sha7);
        return false;
    }
    if is_merge_commit(repo_path, commit_sha) {
        tracing::debug!("tracker filter: {} skipped (merge commit)", sha7);
        return false;
    }
    if is_synthetic_message(repo_path, commit_sha) {
        tracing::debug!("tracker filter: {} skipped (synthetic message)", sha7);
        return false;
    }
    if is_likely_copy_paste(repo_path, commit_sha) {
        tracing::debug!(
            "tracker filter: {} skipped (likely copy-paste, manual additions > {})",
            sha7,
            MANUAL_ADDED_LINES_THRESHOLD
        );
        return false;
    }

    tracing::debug!("tracker filter: {} eligible", sha7);
    true
}

fn is_blacklisted(repo_path: &str, blacklist: &[String]) -> bool {
    blacklist
        .iter()
        .any(|entry| repo_path.contains(entry.as_str()))
}

fn is_merge_commit(repo_path: &str, commit_sha: &str) -> bool {
    let output = Command::new("git")
        .args(["-C", repo_path, "log", "-1", "--format=%P", commit_sha])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let parents = String::from_utf8_lossy(&o.stdout);
            parents.split_whitespace().count() > 1
        }
        _ => false,
    }
}

fn is_synthetic_message(repo_path: &str, commit_sha: &str) -> bool {
    let output = Command::new("git")
        .args(["-C", repo_path, "log", "-1", "--format=%s", commit_sha])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let msg = String::from_utf8_lossy(&o.stdout).to_lowercase();
            let msg = msg.trim();
            msg.starts_with("merge ")
                || msg.starts_with("revert ")
                || msg.starts_with("cherry-pick ")
                || msg.starts_with("rebase ")
                || msg.starts_with("merge pull request")
                || msg.starts_with("merge branch")
                || msg.contains("cherry picked from commit")
        }
        _ => true,
    }
}

#[derive(Deserialize)]
struct GitAIStats {
    git_diff_added_lines: i32,
    ai_additions: i32,
}

fn is_likely_copy_paste(repo_path: &str, commit_sha: &str) -> bool {
    let work_tree = resolve_work_tree(repo_path);

    let binary = resolve_easylife_ai_binary();

    let output = Command::new(&binary)
        .args(["stats", "--json", commit_sha])
        .current_dir(&work_tree)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            match serde_json::from_str::<GitAIStats>(&text) {
                Ok(stats) => {
                    let manual = stats.git_diff_added_lines - stats.ai_additions;
                    manual > MANUAL_ADDED_LINES_THRESHOLD
                }
                Err(e) => {
                    tracing::debug!(
                        "tracker filter: stats parse error for {}: {}",
                        commit_sha,
                        e
                    );
                    false
                }
            }
        }
        Err(e) => {
            tracing::debug!(
                "tracker filter: stats command error for {}: {}",
                commit_sha,
                e
            );
            false
        }
        _ => false,
    }
}

fn resolve_work_tree(repo_path: &str) -> String {
    let path = repo_path.trim_end_matches('/');
    if path.ends_with(".git") {
        path.trim_end_matches(".git")
            .trim_end_matches('/')
            .to_string()
    } else {
        let output = Command::new("git")
            .args(["-C", path, "rev-parse", "--show-toplevel"])
            .output();
        match output {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => path.to_string(),
        }
    }
}

fn resolve_easylife_ai_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("easylife-ai")))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "easylife-ai".to_string())
}
