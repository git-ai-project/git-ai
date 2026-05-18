//! Post-commit worker for the daemon.
//!
//! Processes detected commits by generating authorship notes and writing them.
//! This is the same logic as `handle_post_commit()` in main.rs but takes an
//! explicit repo_path instead of discovering it from CWD.

use std::path::Path;

use crate::core::merge;
use crate::core::post_commit::generate_authorship_for_commit;
use crate::core::working_log;
use crate::git_cmd::git_in_repo;

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
        git_in_repo(
            repo_path,
            &["notes", "--ref=ai", "add", "-f", "-m", &note_text, commit_sha],
        )
        .map_err(|e| format!("git notes add failed for {}: {}", &commit_sha[..7.min(commit_sha.len())], e))?;

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

    // For merge commits, compute attribution from parent notes.
    // This handles both:
    // - Clean merges (no working log data, no note yet)
    // - Conflict merges (working log generated a human-only note that needs
    //   parent attribution data overlaid — fixes #910)
    if let Some(&head_sha) = shas.first()
        && merge::is_merge_commit(repo_path, head_sha)
    {
        // Remove any working-log-based note so compute_merge_attribution can
        // generate a proper one from parent notes. The working log during
        // conflict resolution captures ephemeral edits (marker removal) that
        // don't reflect the true authorship of the final content.
        let _ = git_in_repo(
            repo_path,
            &["notes", "--ref=ai", "remove", head_sha],
        );
        if let Err(e) = merge::compute_merge_attribution(repo_path, head_sha) {
            eprintln!(
                "[git-ai daemon] merge attribution failed for {}: {}",
                &head_sha[..7.min(head_sha.len())],
                e
            );
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
    use std::process::Command;

    #[test]
    fn test_git_in_repo_returns_error_for_bad_dir() {
        let bad_path = PathBuf::from("/nonexistent/path");
        let result = git_in_repo(&bad_path, &["rev-parse", "--git-dir"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_git_in_repo_error_message_includes_command() {
        let bad_path = PathBuf::from("/nonexistent/path");
        let result = git_in_repo(&bad_path, &["log", "--format=%H", "-1"]);
        assert!(result.is_err());
        let err_msg = result.unwrap_err();
        // The error should contain the command args for debugging
        assert!(
            err_msg.contains("log") || err_msg.contains("failed"),
            "error message should be informative: {}",
            err_msg
        );
    }

    #[test]
    fn test_process_commit_nonexistent_repo_returns_error() {
        let bad_path = PathBuf::from("/tmp/nonexistent_repo_for_test_xyz");
        let result = process_commit(&bad_path);
        assert!(
            result.is_err(),
            "process_commit on nonexistent repo should error"
        );
    }

    #[test]
    fn test_process_commit_no_working_log_data_returns_ok_false() {
        // Create a real git repo with a commit but no working log data
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path();

        // Init and create a commit
        Command::new("git")
            .args(["init", repo_path.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(["config", "user.email", "test@test.com"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(["config", "user.name", "Test"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        std::fs::write(repo_path.join("file.txt"), b"hello").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(["add", "."])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(["commit", "-m", "initial"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        // No working log data exists, so process_commit should return Ok(false)
        let result = process_commit(repo_path);
        assert_eq!(
            result,
            Ok(false),
            "no working log data should mean nothing to annotate"
        );
    }
}
