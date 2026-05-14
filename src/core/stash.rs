//! Stash attribution preservation.
//!
//! When `git stash push` saves working changes, we preserve the accumulated
//! working-log attributions as a git note keyed to the stash commit SHA.
//! When `git stash pop/apply` restores those changes, we read the note back
//! and write the attributions as INITIAL state in the working log for the
//! current HEAD.

use std::path::Path;
use std::process::{Command, Stdio};

use super::working_log;

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
// Resolve git dir from repo path
// ---------------------------------------------------------------------------

fn resolve_git_dir(repo_path: &Path) -> Result<std::path::PathBuf, String> {
    let git_dir_str = git_in_repo(repo_path, &["rev-parse", "--git-dir"])?;
    let git_dir_path = std::path::PathBuf::from(&git_dir_str);
    let abs = if git_dir_path.is_relative() {
        repo_path.join(&git_dir_path)
    } else {
        git_dir_path
    };
    Ok(std::fs::canonicalize(&abs).unwrap_or(abs))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Save current working-log attributions as a git note on the stash commit.
///
/// Called after `git stash push/save` completes successfully.
/// 1. Gets the stash SHA via `git rev-parse stash@{0}`
/// 2. Reads existing working log checkpoints for `base_commit`
/// 3. Serializes them as JSON
/// 4. Stores as a git note at `refs/notes/ai-stash`
/// 5. Clears the working log entries for `base_commit`
pub fn save_stash_attributions(repo_path: &Path, base_commit: &str) -> Result<(), String> {
    let git_dir = resolve_git_dir(repo_path)?;

    // Get the stash SHA (the most recent stash entry)
    let stash_sha = git_in_repo(repo_path, &["rev-parse", "stash@{0}"])
        .map_err(|_| "no stash entry found — stash@{0} not available".to_string())?;

    // Read existing checkpoints for this base commit
    let checkpoints = working_log::read_checkpoints(&git_dir, base_commit);

    if checkpoints.is_empty() {
        // Nothing to save
        return Ok(());
    }

    // Also read initial attributions if present
    let initial = working_log::read_initial_attributions(&git_dir, base_commit);

    // Bundle checkpoints + initial into a single JSON payload
    let payload = StashPayload {
        checkpoints,
        initial,
    };

    let json = serde_json::to_string(&payload)
        .map_err(|e| format!("failed to serialize stash payload: {}", e))?;

    // Store as a git note on the stash commit
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args([
            "notes",
            "--ref=ai-stash",
            "add",
            "-f",
            "-m",
            &json,
            &stash_sha,
        ])
        .env("GIT_TRACE2_EVENT", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .map_err(|e| format!("failed to run git notes: {}", e))?;

    if !status.success() {
        return Err("git notes --ref=ai-stash add failed".to_string());
    }

    // Clear the working log for this base commit
    working_log::delete_working_log(&git_dir, base_commit);

    eprintln!(
        "[git-ai daemon] stash: saved attributions for {} on stash {}",
        &base_commit[..7.min(base_commit.len())],
        &stash_sha[..7.min(stash_sha.len())]
    );

    Ok(())
}

/// Restore working-log attributions from a stash note.
///
/// Called after `git stash pop/apply` completes successfully.
/// 1. Gets the stash SHA from the most recent stash operation
/// 2. Reads the note from `refs/notes/ai-stash`
/// 3. Parses the JSON back into checkpoint data
/// 4. Writes those checkpoints to the working log for the current HEAD
pub fn restore_stash_attributions(repo_path: &Path, base_commit: &str) -> Result<(), String> {
    let stash_sha = find_applied_stash_sha(repo_path)?;
    restore_stash_attributions_for_sha(repo_path, base_commit, &stash_sha)
}

/// Restore working-log attributions from a specific stash SHA.
pub fn restore_stash_attributions_for_sha(
    repo_path: &Path,
    base_commit: &str,
    stash_sha: &str,
) -> Result<(), String> {
    let git_dir = resolve_git_dir(repo_path)?;

    // Read the note
    let note_content = git_in_repo(repo_path, &["notes", "--ref=ai-stash", "show", stash_sha])
        .map_err(|_| {
            format!(
                "no ai-stash note found for stash {}",
                &stash_sha[..7.min(stash_sha.len())]
            )
        })?;

    // Parse the payload
    let payload: StashPayload = serde_json::from_str(&note_content)
        .map_err(|e| format!("failed to parse stash payload: {}", e))?;

    // Write checkpoints to the working log for the current HEAD (base_commit)
    for checkpoint in &payload.checkpoints {
        working_log::append_checkpoint(&git_dir, base_commit, checkpoint);
    }

    // Write initial attributions if present
    if let Some(ref initial) = payload.initial {
        working_log::write_initial_attributions(&git_dir, base_commit, initial);
    }

    eprintln!(
        "[git-ai daemon] stash: restored attributions from stash {} to {}",
        &stash_sha[..7.min(stash_sha.len())],
        &base_commit[..7.min(base_commit.len())]
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Find the SHA of the stash that was just applied/popped.
///
/// For `apply`: stash@{0} still exists and has the note.
/// For `pop`: the entry was dropped, so we check the stash reflog.
fn find_applied_stash_sha(repo_path: &Path) -> Result<String, String> {
    // First try stash@{0} — works for `apply` where the stash entry remains
    if let Ok(sha) = git_in_repo(repo_path, &["rev-parse", "stash@{0}"]) {
        // Check if this stash has an ai-stash note
        if git_in_repo(repo_path, &["notes", "--ref=ai-stash", "show", &sha]).is_ok() {
            return Ok(sha);
        }
    }

    // For `pop`, the stash entry was removed. Check the stash reflog for the
    // most recently dropped entry.
    let reflog = git_in_repo(
        repo_path,
        &["reflog", "show", "refs/stash", "--format=%H", "-1"],
    );

    if let Ok(sha) = reflog {
        if !sha.is_empty() {
            return Ok(sha);
        }
    }

    // Last resort: if stash reflog is gone (last stash was popped), try
    // looking at the reflog for HEAD to find which commit was the stash
    // This handles the edge case where popping the only stash deletes refs/stash entirely.
    // In that case, we can look for the stash commit in the dangling objects,
    // but that's complex. For now, check if there are any notes in ai-stash
    // and try to find the most recent one.
    let notes_list = git_in_repo(repo_path, &["notes", "--ref=ai-stash", "list"]);
    if let Ok(list) = notes_list {
        // Format: "<note-blob-sha> <annotated-object-sha>"
        // Take the last one (most recently added)
        if let Some(last_line) = list.lines().last() {
            let parts: Vec<&str> = last_line.split_whitespace().collect();
            if parts.len() >= 2 {
                return Ok(parts[1].to_string());
            }
        }
    }

    Err("could not determine stash SHA for restore".to_string())
}

// ---------------------------------------------------------------------------
// Payload types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StashPayload {
    checkpoints: Vec<working_log::Checkpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initial: Option<working_log::InitialAttributions>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::working_log::{Checkpoint, CheckpointKind, WorkingLogEntry};
    use std::path::PathBuf;

    fn setup_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().to_path_buf();

        Command::new("git")
            .args(["init", repo_path.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["config", "user.name", "Test"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["config", "user.email", "t@t.com"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        (dir, repo_path)
    }

    fn commit_file(repo_path: &Path, name: &str, content: &str, msg: &str) -> String {
        std::fs::write(repo_path.join(name), content).unwrap();
        Command::new("git")
            .current_dir(repo_path)
            .args(["add", name])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(repo_path)
            .args(["commit", "-m", msg])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        git_in_repo(repo_path, &["rev-parse", "HEAD"]).unwrap()
    }

    #[test]
    fn test_save_and_restore_stash_attributions() {
        let (_dir, repo_path) = setup_repo();
        let git_dir = resolve_git_dir(&repo_path).unwrap();

        // Create initial commit
        let base_sha = commit_file(&repo_path, "file.txt", "initial\n", "initial commit");

        // Write some working log data for the base commit
        let entry = WorkingLogEntry {
            file: "file.txt".into(),
            blob_sha: "abc123".into(),
            attributions: vec![],
            line_attributions: vec![],
        };
        let checkpoint = Checkpoint::new(CheckpointKind::AiAgent, "claude".into(), vec![entry]);
        working_log::append_checkpoint(&git_dir, &base_sha, &checkpoint);

        // Verify working log exists
        let loaded = working_log::read_checkpoints(&git_dir, &base_sha);
        assert_eq!(loaded.len(), 1);

        // Make a change and stash it
        std::fs::write(repo_path.join("file.txt"), "modified\n").unwrap();
        let stash_output = Command::new("git")
            .current_dir(&repo_path)
            .args(["stash", "push", "-m", "test stash"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        assert!(stash_output.status.success());

        // Save attributions
        let result = save_stash_attributions(&repo_path, &base_sha);
        assert!(result.is_ok(), "save failed: {:?}", result.err());

        // Verify working log was cleared
        let loaded = working_log::read_checkpoints(&git_dir, &base_sha);
        assert!(loaded.is_empty());

        // Pop the stash
        let pop_output = Command::new("git")
            .current_dir(&repo_path)
            .args(["stash", "apply"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        assert!(pop_output.status.success());

        // Restore attributions
        let current_head = git_in_repo(&repo_path, &["rev-parse", "HEAD"]).unwrap();
        let result = restore_stash_attributions(&repo_path, &current_head);
        assert!(result.is_ok(), "restore failed: {:?}", result.err());

        // Verify working log was restored
        let restored = working_log::read_checkpoints(&git_dir, &current_head);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].author, "claude");
        assert_eq!(restored[0].entries[0].file, "file.txt");
    }

    #[test]
    fn test_save_with_no_working_log_is_noop() {
        let (_dir, repo_path) = setup_repo();

        // Create initial commit
        let base_sha = commit_file(&repo_path, "file.txt", "initial\n", "initial commit");

        // Make a change and stash it
        std::fs::write(repo_path.join("file.txt"), "modified\n").unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["stash", "push"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        // Save with no working log data should succeed (noop)
        let result = save_stash_attributions(&repo_path, &base_sha);
        assert!(result.is_ok());
    }
}
