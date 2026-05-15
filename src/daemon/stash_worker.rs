//! Stash worker for the daemon.
//!
//! Handles `git stash push` and `git stash pop/apply` events by delegating to
//! `core::stash` to save/restore working-log attributions.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::core::stash;

// ---------------------------------------------------------------------------
// Git helper
// ---------------------------------------------------------------------------

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

/// Process a detected `git stash push/save`.
///
/// Determines the current HEAD (base commit for working log) and saves
/// accumulated attributions as a note on the stash commit.
/// The `argv` parameter carries the original stash command arguments so we
/// can extract pathspecs (everything after `--`) and filter working log entries.
pub fn process_stash_push(repo_path: &Path, argv: &[String]) -> Result<(), String> {
    let base_commit = git_in_repo(repo_path, &["rev-parse", "HEAD"])
        .map_err(|e| format!("cannot determine HEAD: {}", e))?;

    let pathspecs = stash::extract_pathspecs_from_argv(argv);
    stash::save_stash_attributions(repo_path, &base_commit, &pathspecs)
}

/// Process a detected `git stash pop/apply`.
///
/// Determines the current HEAD and restores attributions from the stash note
/// into the working log.
pub fn process_stash_pop(repo_path: &Path) -> Result<(), String> {
    let base_commit = git_in_repo(repo_path, &["rev-parse", "HEAD"])
        .map_err(|e| format!("cannot determine HEAD: {}", e))?;

    stash::restore_stash_attributions(repo_path, &base_commit)
}
