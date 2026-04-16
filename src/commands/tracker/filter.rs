use crate::commands::tracker::notes;
use std::process::Command;

pub fn should_upload(repo_path: &str, commit_sha: &str, blacklist: &[String]) -> bool {
    if is_blacklisted(repo_path, blacklist) {
        return false;
    }
    if notes::is_already_reported(repo_path, commit_sha) {
        return false;
    }
    if !has_single_parent(repo_path, commit_sha) {
        return false;
    }
    if !author_matches_committer(repo_path, commit_sha) {
        return false;
    }
    if is_synthetic_message(repo_path, commit_sha) {
        return false;
    }
    true
}

fn is_blacklisted(repo_path: &str, blacklist: &[String]) -> bool {
    blacklist
        .iter()
        .any(|entry| repo_path.contains(entry.as_str()))
}

fn has_single_parent(repo_path: &str, commit_sha: &str) -> bool {
    let output = Command::new("git")
        .args(["-C", repo_path, "show", "-s", "--format=%P", commit_sha])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let parents = String::from_utf8_lossy(&o.stdout);
            parents.trim().split_whitespace().count() == 1
        }
        _ => false,
    }
}

fn author_matches_committer(repo_path: &str, commit_sha: &str) -> bool {
    let output = Command::new("git")
        .args([
            "-C",
            repo_path,
            "show",
            "-s",
            "--format=%ae%n%ce",
            commit_sha,
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let mut lines = text.trim().lines();
            let author = lines.next().unwrap_or("");
            let committer = lines.next().unwrap_or("");
            author == committer
        }
        _ => false,
    }
}

fn is_synthetic_message(repo_path: &str, commit_sha: &str) -> bool {
    let output = Command::new("git")
        .args(["-C", repo_path, "show", "-s", "--format=%B", commit_sha])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let msg = String::from_utf8_lossy(&o.stdout).to_lowercase();
            msg.contains("cherry-pick") || msg.contains("revert")
        }
        _ => true,
    }
}
