//! Stash worker for the daemon.
//!
//! Handles `git stash push` and `git stash pop/apply` events by delegating to
//! `core::stash` to save/restore working-log attributions.

use std::path::Path;

use crate::core::stash;
use crate::git_cmd::git_in_repo;

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
