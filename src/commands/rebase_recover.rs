//! Attribution recovery command for rebases that lost notes.
//!
//! This module provides the `git-ai rebase recover` command, which restores
//! attribution from orphaned notes or backup snapshots.

use crate::error::GitAiError;
use crate::git::repository::Repository;

/// Recover attribution after a rebase that lost notes.
///
/// This command attempts to restore attribution by:
/// 1. Reading orphaned notes from refs/git-ai/orphaned-notes/<original-head>
/// 2. Mapping original commits to current HEAD commits by content
/// 3. Copying notes from original commits to new commits
///
/// # Arguments
/// * `repo` - Git repository
/// * `original_head` - SHA of original HEAD before rebase (source of orphaned notes)
pub fn recover_attribution(repo: &Repository, original_head: &str) -> Result<(), GitAiError> {
    if original_head.len() < 8 {
        return Err(GitAiError::Generic(format!(
            "SHA too short (need at least 8 characters): '{}'",
            original_head
        )));
    }

    eprintln!(
        "[git-ai] Attempting attribution recovery from {}...",
        &original_head[..8]
    );

    // Step 1: Find orphaned notes ref
    let orphaned_ref = format!("refs/git-ai/orphaned-notes/{}", original_head);

    // Check if orphaned notes exist
    let ref_check = check_ref_exists(repo, &orphaned_ref)?;
    if !ref_check {
        // Try backup refs if orphaned notes don't exist
        return recover_from_backup_refs(repo, original_head);
    }

    // Step 2: Get current HEAD
    let current_head = repo.head()?.target()?;

    // Step 3: Get original commits (walk from original_head)
    let original_commits = get_commits_from_head(repo, original_head)?;

    if original_commits.is_empty() {
        return Err(GitAiError::Generic(format!(
            "No commits found at original HEAD {}",
            original_head
        )));
    }

    // Step 4: Get current commits (walk from current HEAD)
    let current_commits = get_commits_from_head(repo, &current_head)?;

    if current_commits.is_empty() {
        return Err(GitAiError::Generic(
            "No commits found at current HEAD".to_string(),
        ));
    }

    eprintln!(
        "[git-ai] Found {} original commits, {} current commits",
        original_commits.len(),
        current_commits.len()
    );

    // Step 5: Map original commits to current commits by content
    let commit_pairs = map_commits_by_content(repo, &original_commits, &current_commits)?;

    if commit_pairs.is_empty() {
        return Err(GitAiError::Generic(
            "Could not map any commits (content too different)".to_string(),
        ));
    }

    eprintln!(
        "[git-ai] Mapped {} commit pairs by content similarity",
        commit_pairs.len()
    );

    // Step 6: Copy notes from original commits to current commits
    let mut recovered_count = 0;
    for (original_sha, current_sha) in &commit_pairs {
        if copy_note_if_exists(repo, original_sha, current_sha)? {
            recovered_count += 1;
        }
    }

    if recovered_count == 0 {
        eprintln!("[git-ai] No notes found to recover");
        return Ok(());
    }

    eprintln!(
        "[git-ai] ✓ Successfully recovered attribution for {} commits",
        recovered_count
    );

    // Step 7: Clean up orphaned notes ref
    let _ = delete_ref(repo, &orphaned_ref);

    Ok(())
}

/// Recover attribution from backup snapshots.
fn recover_from_backup_refs(repo: &Repository, _original_head: &str) -> Result<(), GitAiError> {
    eprintln!("[git-ai] Looking for backup snapshots...");

    // List all backup refs
    let backup_refs = list_refs_with_prefix(repo, "refs/git-ai/backup/")?;

    if backup_refs.is_empty() {
        return Err(GitAiError::Generic(
            "No orphaned notes or backup snapshots found".to_string(),
        ));
    }

    // Sort by timestamp (newest first)
    let mut refs_with_timestamps: Vec<(String, u64)> = backup_refs
        .iter()
        .filter_map(|ref_name| {
            // Extract timestamp from refs/git-ai/backup/notes-<timestamp>
            let parts: Vec<&str> = ref_name.split('-').collect();
            if let Some(ts_str) = parts.last()
                && let Ok(ts) = ts_str.parse::<u64>()
            {
                return Some((ref_name.clone(), ts));
            }
            None
        })
        .collect();

    refs_with_timestamps.sort_by_key(|b| std::cmp::Reverse(b.1)); // Newest first

    if refs_with_timestamps.is_empty() {
        return Err(GitAiError::Generic(
            "Found backup refs but could not parse timestamps".to_string(),
        ));
    }

    let (newest_ref, timestamp) = &refs_with_timestamps[0];
    eprintln!(
        "[git-ai] Found backup snapshot from timestamp {}",
        timestamp
    );
    eprintln!("[git-ai] Merging notes from {}", newest_ref);

    // Merge notes from backup ref
    merge_notes_from_ref(repo, newest_ref)?;

    eprintln!("[git-ai] ✓ Successfully merged notes from backup");

    Ok(())
}

/// Check if a Git ref exists.
fn check_ref_exists(repo: &Repository, ref_name: &str) -> Result<bool, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "show-ref".to_string(),
        "--verify".to_string(),
        "--quiet".to_string(),
        ref_name.to_string(),
    ]);

    match crate::git::repository::exec_git(&args) {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// List all refs with a given prefix.
fn list_refs_with_prefix(repo: &Repository, prefix: &str) -> Result<Vec<String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "for-each-ref".to_string(),
        "--format=%(refname)".to_string(),
        prefix.to_string(),
    ]);

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)?;

    Ok(stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.to_string())
        .collect())
}

/// Get commits from a given HEAD (up to 100 commits).
fn get_commits_from_head(repo: &Repository, head: &str) -> Result<Vec<String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "rev-list".to_string(),
        "--max-count=100".to_string(),
        head.to_string(),
    ]);

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)?;

    Ok(stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .collect())
}

/// Map original commits to current commits by content similarity (commit message).
fn map_commits_by_content(
    repo: &Repository,
    original_commits: &[String],
    current_commits: &[String],
) -> Result<Vec<(String, String)>, GitAiError> {
    // Build map of commit message -> SHA for current commits
    let mut current_subjects = std::collections::HashMap::new();
    for sha in current_commits {
        let subject = get_commit_subject(repo, sha)?;
        current_subjects.insert(subject, sha.clone());
    }

    let mut pairs = Vec::new();

    // For each original commit, try to find a current commit with same subject
    for original_sha in original_commits {
        let original_subject = get_commit_subject(repo, original_sha)?;

        if let Some(current_sha) = current_subjects.get(&original_subject) {
            pairs.push((original_sha.clone(), current_sha.clone()));
        }
    }

    Ok(pairs)
}

/// Get commit subject (first line of commit message).
fn get_commit_subject(repo: &Repository, sha: &str) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "log".to_string(),
        "--format=%s".to_string(),
        "-n".to_string(),
        "1".to_string(),
        sha.to_string(),
    ]);

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)?;

    Ok(stdout.trim().to_string())
}

/// Copy note from original commit to current commit if it exists.
/// Returns true if note was copied, false if no note existed.
fn copy_note_if_exists(
    repo: &Repository,
    original_sha: &str,
    current_sha: &str,
) -> Result<bool, GitAiError> {
    // Check if original has a note
    let note_content = match crate::git::refs::show_authorship_note(repo, original_sha) {
        Some(content) => content,
        None => return Ok(false),
    };

    // Copy note to current commit
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "notes".to_string(),
        "--ref=ai".to_string(),
        "add".to_string(),
        "-f".to_string(),
        "-m".to_string(),
        note_content,
        current_sha.to_string(),
    ]);

    crate::git::repository::exec_git(&args)?;

    Ok(true)
}

/// Merge notes from a ref into refs/notes/ai.
fn merge_notes_from_ref(repo: &Repository, ref_name: &str) -> Result<(), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "notes".to_string(),
        "--ref=ai".to_string(),
        "merge".to_string(),
        "-s".to_string(),
        "ours".to_string(), // Keep our notes if conflict
        ref_name.to_string(),
    ]);

    crate::git::repository::exec_git(&args)?;

    Ok(())
}

/// Delete a Git ref.
fn delete_ref(repo: &Repository, ref_name: &str) -> Result<(), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "update-ref".to_string(),
        "-d".to_string(),
        ref_name.to_string(),
    ]);

    crate::git::repository::exec_git(&args)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn test_commit_subject_parsing() {
        // Test that commit subject extraction would work
        // (actual implementation requires real git repo)
    }
}
