//! Post-commit worker for the daemon.
//!
//! Processes detected commits by generating authorship notes and writing them.
//! This is the same logic as `handle_post_commit()` in main.rs but takes an
//! explicit repo_path instead of discovering it from CWD.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::core::merge;
use crate::core::post_commit::generate_authorship_for_commit;
use crate::core::working_log;

// ---------------------------------------------------------------------------
// Git helper
// ---------------------------------------------------------------------------

/// Run a git command in the given repo with GIT_TRACE2_EVENT=0 to prevent
/// recursive daemon events.
fn git_in_repo(repo_path: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .env("GIT_TRACE2_EVENT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("git failed to execute: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git {} failed: {}", args.join(" "), stderr))
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Process a detected commit: generate authorship notes for unannotated commits.
///
/// Scans recent commits in the repo to find any that have working log data
/// but no authorship note yet. This handles the race condition where multiple
/// commits happen faster than the daemon can process their trace2 events.
///
/// Returns `Ok(true)` if at least one note was written, `Ok(false)` if all skipped.
pub fn process_commit(repo_path: &Path) -> Result<bool, String> {
    let git_dir_str = git_in_repo(repo_path, &["rev-parse", "--git-dir"])?;
    let git_dir_path = std::path::PathBuf::from(&git_dir_str);
    let git_dir_abs = if git_dir_path.is_relative() {
        repo_path.join(&git_dir_path)
    } else {
        git_dir_path
    };
    let git_dir = std::fs::canonicalize(&git_dir_abs).unwrap_or(git_dir_abs);

    let repo_dir = git_in_repo(repo_path, &["rev-parse", "--show-toplevel"])
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| repo_path.to_path_buf());

    // Get recent commits (up to 10) to check for unannotated ones with working log data
    let log_output = git_in_repo(repo_path, &["log", "--format=%H", "-10"])?;
    let shas: Vec<&str> = log_output.lines().collect();

    let mut wrote_any = false;

    for (i, &commit_sha) in shas.iter().enumerate() {
        // Skip if note already exists
        if git_in_repo(repo_path, &["notes", "--ref=ai", "show", commit_sha]).is_ok() {
            continue;
        }

        // Determine parent SHA
        let parent_sha = if i + 1 < shas.len() {
            shas[i + 1].to_string()
        } else {
            git_in_repo(repo_path, &["rev-parse", &format!("{}~1", commit_sha)])
                .unwrap_or_else(|_| "initial".to_string())
        };

        // Check if working log exists for this parent
        let working_log_dir = git_dir.join("ai").join("working_logs").join(&parent_sha);
        if !working_log_dir.exists() {
            continue;
        }

        let human_author = git_in_repo(repo_path, &["log", "-1", "--format=%aN <%aE>", commit_sha])
            .unwrap_or_else(|_| "Unknown <unknown>".to_string());

        let (authorship_log, initial_attrs) = generate_authorship_for_commit(
            &git_dir,
            &repo_dir,
            &parent_sha,
            commit_sha,
            &human_author,
        )
        .map_err(|e| format!("generate_authorship_for_commit failed: {}", e))?;

        let note_text = authorship_log.serialize_to_string();
        let status = Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args([
                "notes", "--ref=ai", "add", "-f", "-m", &note_text, commit_sha,
            ])
            .env("GIT_TRACE2_EVENT", "0")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status()
            .map_err(|e| format!("failed to run git notes: {}", e))?;

        if !status.success() {
            return Err(format!(
                "git notes add failed for {}",
                &commit_sha[..7.min(commit_sha.len())]
            ));
        }

        eprintln!(
            "[git-ai daemon] wrote authorship note for {}",
            &commit_sha[..7.min(commit_sha.len())]
        );

        // Write marker so the post-commit hook knows not to duplicate work
        let noted_dir = git_dir.join("ai").join("noted");
        let _ = std::fs::create_dir_all(&noted_dir);
        let _ = std::fs::write(noted_dir.join(commit_sha), b"");

        if let Some(initial) = initial_attrs {
            working_log::write_initial_attributions(&git_dir, commit_sha, &initial);
        }

        working_log::delete_working_log(&git_dir, &parent_sha);
        wrote_any = true;
    }

    // After processing regular commits, check if HEAD is a merge commit
    // that still lacks a note. This handles merges where both parents have
    // authorship notes but no working log data was accumulated (non-conflicting merges).
    if let Some(&head_sha) = shas.first() {
        if merge::is_merge_commit(repo_path, head_sha) {
            // compute_merge_attribution is a no-op if a note already exists
            if let Err(e) = merge::compute_merge_attribution(repo_path, head_sha) {
                eprintln!(
                    "[git-ai daemon] merge attribution failed for {}: {}",
                    &head_sha[..7.min(head_sha.len())],
                    e
                );
            }
        }
    }

    Ok(wrote_any)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_git_in_repo_returns_error_for_bad_dir() {
        let bad_path = PathBuf::from("/nonexistent/path");
        let result = git_in_repo(&bad_path, &["rev-parse", "--git-dir"]);
        assert!(result.is_err());
    }
}
