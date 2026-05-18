//! Stash attribution preservation.
//!
//! When `git stash push` saves working changes, we preserve the accumulated
//! working-log attributions as a git note keyed to the stash commit SHA.
//! When `git stash pop/apply` restores those changes, we read the note back
//! and write the attributions as INITIAL state in the working log for the
//! current HEAD.

use std::path::Path;

use super::working_log;
use crate::git_cmd::git_in_repo;

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
// Pathspec matching
// ---------------------------------------------------------------------------

/// Check whether a file path matches any of the given pathspecs.
///
/// Supports:
/// - Exact paths: `src/main.rs`
/// - Directory prefixes: `src/` (matches all files under `src/`)
/// - Glob patterns: `*.rs`, `src/**/*.rs`
///
/// An empty pathspecs list matches everything.
pub fn matches_pathspec(file_path: &str, pathspecs: &[String]) -> bool {
    if pathspecs.is_empty() {
        return true;
    }

    for spec in pathspecs {
        // Exact match
        if file_path == spec {
            return true;
        }

        // Directory prefix match (e.g., "src/" matches "src/main.rs")
        if spec.ends_with('/') && file_path.starts_with(spec.as_str()) {
            return true;
        }

        // Directory prefix without trailing slash (e.g., "src" matches "src/main.rs")
        if !spec.contains('*') && !spec.contains('?') && !spec.contains('[') {
            let with_slash = format!("{}/", spec);
            if file_path.starts_with(&with_slash) {
                return true;
            }
        }

        // Glob pattern matching
        if let Ok(pattern) = glob::Pattern::new(spec) {
            let opts = glob::MatchOptions {
                case_sensitive: true,
                require_literal_separator: false,
                require_literal_leading_dot: false,
            };
            if pattern.matches_with(file_path, opts) {
                return true;
            }
        }
    }

    false
}

/// Extract pathspecs from stash command argv.
///
/// In `git stash push -- <pathspec>...`, everything after the `--` separator
/// is a pathspec. Returns an empty vec if no `--` is found.
pub fn extract_pathspecs_from_argv(argv: &[String]) -> Vec<String> {
    if let Some(separator_pos) = argv.iter().position(|a| a == "--") {
        argv[separator_pos + 1..].to_vec()
    } else {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Save current working-log attributions as a git note on the stash commit.
///
/// Called after `git stash push/save` completes successfully.
/// 1. Gets the stash SHA via `git rev-parse stash@{0}`
/// 2. Reads existing working log checkpoints for `base_commit`
/// 3. Filters entries by pathspec if pathspecs are provided
/// 4. Serializes them as JSON
/// 5. Stores as a git note at `refs/notes/ai-stash`
/// 6. Clears the working log entries for `base_commit` (only matched entries if filtered)
pub fn save_stash_attributions(
    repo_path: &Path,
    base_commit: &str,
    pathspecs: &[String],
) -> Result<(), String> {
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

    // Filter checkpoints and initial attributions by pathspec
    let (filtered_checkpoints, filtered_initial) = if pathspecs.is_empty() {
        (checkpoints.clone(), initial.clone())
    } else {
        let filtered_cps = filter_checkpoints_by_pathspec(&checkpoints, pathspecs);
        let filtered_init = initial
            .as_ref()
            .map(|init| filter_initial_by_pathspec(init, pathspecs));
        (filtered_cps, filtered_init)
    };

    if filtered_checkpoints.is_empty() && filtered_initial.is_none() {
        // No matching entries to save
        return Ok(());
    }

    // Bundle checkpoints + initial into a single JSON payload
    let payload = StashPayload {
        checkpoints: filtered_checkpoints,
        initial: filtered_initial,
    };

    let json = serde_json::to_string(&payload)
        .map_err(|e| format!("failed to serialize stash payload: {}", e))?;

    // Store as a git note on the stash commit
    git_in_repo(
        repo_path,
        &["notes", "--ref=ai-stash", "add", "-f", "-m", &json, &stash_sha],
    )
    .map_err(|_| "git notes --ref=ai-stash add failed".to_string())?;

    // Clear or update the working log for this base commit
    if pathspecs.is_empty() {
        // No pathspec filter: clear the entire working log
        working_log::delete_working_log(&git_dir, base_commit);
    } else {
        // Pathspec filter: only remove entries that matched, keep the rest
        let remaining_checkpoints = filter_checkpoints_excluding_pathspec(&checkpoints, pathspecs);
        let remaining_initial = initial
            .as_ref()
            .map(|init| filter_initial_excluding_pathspec(init, pathspecs));

        // Rewrite the working log with only the remaining entries
        working_log::delete_working_log(&git_dir, base_commit);
        for cp in &remaining_checkpoints {
            working_log::append_checkpoint(&git_dir, base_commit, cp);
        }
        if let Some(ref init) = remaining_initial
            && !init.files.is_empty()
        {
            working_log::write_initial_attributions(&git_dir, base_commit, init);
        }
    }

    eprintln!(
        "[git-ai daemon] stash: saved attributions for {} on stash {}",
        &base_commit[..7.min(base_commit.len())],
        &stash_sha[..7.min(stash_sha.len())]
    );

    Ok(())
}

/// Save the current stash@{0} SHA to a temp file for later restoration.
///
/// Called by the pre-hook before `git stash pop/apply` to record which stash
/// is about to be popped. This solves the bug where after pop, stash@{0} points
/// to the next stash, not the one that was just popped.
pub fn save_stash_sha_before_pop(repo_path: &Path) -> Result<(), String> {
    let stash_sha = git_in_repo(repo_path, &["rev-parse", "stash@{0}"])
        .map_err(|_| "no stash entry found — stash@{0} not available".to_string())?;

    let git_dir = resolve_git_dir(repo_path)?;
    let ai_dir = git_dir.join("ai");
    std::fs::create_dir_all(&ai_dir)
        .map_err(|e| format!("failed to create .git/ai directory: {}", e))?;

    let last_stash_file = ai_dir.join("last_stash_sha");
    std::fs::write(&last_stash_file, &stash_sha)
        .map_err(|e| format!("failed to write last_stash_sha: {}", e))?;

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
/// For `pop`: the entry was dropped, so we check a pre-saved SHA file.
///
/// BUG FIX: After `git stash pop`, the reflog tip is the NEXT stash, not the popped one.
/// The correct approach is for the pre-hook to save the SHA before popping.
/// For now, we check a temp file `.git/ai/last_stash_sha` that should be written by
/// the pre-hook, or fall back to heuristics.
fn find_applied_stash_sha(repo_path: &Path) -> Result<String, String> {
    // First try stash@{0} — works for `apply` where the stash entry remains
    if let Ok(sha) = git_in_repo(repo_path, &["rev-parse", "stash@{0}"]) {
        // Check if this stash has an ai-stash note
        if git_in_repo(repo_path, &["notes", "--ref=ai-stash", "show", &sha]).is_ok() {
            return Ok(sha);
        }
    }

    // For `pop`, check if the pre-hook saved the stash SHA
    let git_dir_str = git_in_repo(repo_path, &["rev-parse", "--git-dir"])?;
    let git_dir = std::path::PathBuf::from(&git_dir_str);
    let abs_git_dir = if git_dir.is_relative() {
        repo_path.join(&git_dir)
    } else {
        git_dir
    };
    let last_stash_file = abs_git_dir.join("ai").join("last_stash_sha");

    if let Ok(saved_sha) = std::fs::read_to_string(&last_stash_file) {
        let sha = saved_sha.trim().to_string();
        // Clean up the temp file
        let _ = std::fs::remove_file(&last_stash_file);
        if !sha.is_empty() {
            return Ok(sha);
        }
    }

    // Fallback heuristic: Check HEAD reflog for stash application
    // When stash pop/apply happens, git writes an entry like "WIP on <branch>: <sha>"
    // to HEAD's reflog. We look for that pattern.
    let head_reflog = git_in_repo(
        repo_path,
        &["reflog", "show", "HEAD", "--format=%H %gs", "-5"],
    );
    if let Ok(reflog) = head_reflog {
        for line in reflog.lines() {
            // Look for entries that mention "WIP on" or "stash"
            if line.contains("WIP on") || line.to_lowercase().contains("stash") {
                // Extract the SHA from the first word
                if let Some(sha) = line.split_whitespace().next() {
                    // Verify this is a valid stash commit with an ai-stash note
                    if git_in_repo(repo_path, &["notes", "--ref=ai-stash", "show", sha]).is_ok() {
                        return Ok(sha.to_string());
                    }
                }
            }
        }
    }

    // Last resort: search all ai-stash notes and try to find the most recent one
    // This is a known limitation — without pre-hook coordination, we can't reliably
    // determine which stash was popped when multiple stashes exist.
    let notes_list = git_in_repo(repo_path, &["notes", "--ref=ai-stash", "list"]);
    if let Ok(list) = notes_list {
        // Format: "<note-blob-sha> <annotated-object-sha>"
        // Take the last one (most recently added)
        if let Some(last_line) = list.lines().last() {
            let parts: Vec<&str> = last_line.split_whitespace().collect();
            if parts.len() >= 2 {
                // TODO: This is unreliable when multiple stashes exist.
                // Proper fix requires pre-hook to save the stash@{0} SHA before pop.
                return Ok(parts[1].to_string());
            }
        }
    }

    Err("could not determine stash SHA for restore — consider using 'git stash apply' instead of 'pop', or ensure pre-hook is properly configured".to_string())
}

/// Filter checkpoints to only include entries whose file paths match the pathspecs.
fn filter_checkpoints_by_pathspec(
    checkpoints: &[working_log::Checkpoint],
    pathspecs: &[String],
) -> Vec<working_log::Checkpoint> {
    checkpoints
        .iter()
        .filter_map(|cp| {
            let filtered_entries: Vec<working_log::WorkingLogEntry> = cp
                .entries
                .iter()
                .filter(|entry| matches_pathspec(&entry.file, pathspecs))
                .cloned()
                .collect();
            if filtered_entries.is_empty() {
                None
            } else {
                let mut new_cp = cp.clone();
                new_cp.entries = filtered_entries;
                Some(new_cp)
            }
        })
        .collect()
}

/// Filter checkpoints to EXCLUDE entries whose file paths match the pathspecs
/// (keep only non-matching entries).
fn filter_checkpoints_excluding_pathspec(
    checkpoints: &[working_log::Checkpoint],
    pathspecs: &[String],
) -> Vec<working_log::Checkpoint> {
    checkpoints
        .iter()
        .filter_map(|cp| {
            let remaining_entries: Vec<working_log::WorkingLogEntry> = cp
                .entries
                .iter()
                .filter(|entry| !matches_pathspec(&entry.file, pathspecs))
                .cloned()
                .collect();
            if remaining_entries.is_empty() {
                None
            } else {
                let mut new_cp = cp.clone();
                new_cp.entries = remaining_entries;
                Some(new_cp)
            }
        })
        .collect()
}

/// Filter initial attributions to only include files matching pathspecs.
fn filter_initial_by_pathspec(
    initial: &working_log::InitialAttributions,
    pathspecs: &[String],
) -> working_log::InitialAttributions {
    let mut filtered = initial.clone();
    filtered
        .files
        .retain(|file_path, _| matches_pathspec(file_path, pathspecs));
    filtered
}

/// Filter initial attributions to EXCLUDE files matching pathspecs.
fn filter_initial_excluding_pathspec(
    initial: &working_log::InitialAttributions,
    pathspecs: &[String],
) -> working_log::InitialAttributions {
    let mut filtered = initial.clone();
    filtered
        .files
        .retain(|file_path, _| !matches_pathspec(file_path, pathspecs));
    filtered
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
    use std::process::{Command, Stdio};

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

        // Save attributions (no pathspec)
        let result = save_stash_attributions(&repo_path, &base_sha, &[]);
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
        let result = save_stash_attributions(&repo_path, &base_sha, &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_matches_pathspec_exact() {
        assert!(matches_pathspec(
            "src/main.rs",
            &["src/main.rs".to_string()]
        ));
        assert!(!matches_pathspec(
            "src/lib.rs",
            &["src/main.rs".to_string()]
        ));
    }

    #[test]
    fn test_matches_pathspec_directory_prefix() {
        assert!(matches_pathspec("src/main.rs", &["src/".to_string()]));
        assert!(matches_pathspec("src/lib.rs", &["src/".to_string()]));
        assert!(!matches_pathspec("tests/test.rs", &["src/".to_string()]));
    }

    #[test]
    fn test_matches_pathspec_directory_no_trailing_slash() {
        assert!(matches_pathspec("src/main.rs", &["src".to_string()]));
        assert!(matches_pathspec("src/sub/file.rs", &["src".to_string()]));
        assert!(!matches_pathspec("tests/test.rs", &["src".to_string()]));
    }

    #[test]
    fn test_matches_pathspec_glob() {
        assert!(matches_pathspec("src/main.rs", &["*.rs".to_string()]));
        assert!(matches_pathspec("src/main.rs", &["src/*.rs".to_string()]));
        assert!(!matches_pathspec("src/main.rs", &["*.txt".to_string()]));
    }

    #[test]
    fn test_matches_pathspec_empty_matches_all() {
        assert!(matches_pathspec("anything/at/all.txt", &[]));
    }

    #[test]
    fn test_matches_pathspec_multiple_specs() {
        let specs = vec!["src/".to_string(), "*.md".to_string()];
        assert!(matches_pathspec("src/main.rs", &specs));
        assert!(matches_pathspec("README.md", &specs));
        assert!(!matches_pathspec("tests/test.rs", &specs));
    }

    #[test]
    fn test_extract_pathspecs_from_argv() {
        let argv = vec![
            "git".to_string(),
            "stash".to_string(),
            "push".to_string(),
            "--".to_string(),
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
        ];
        let specs = extract_pathspecs_from_argv(&argv);
        assert_eq!(specs, vec!["src/main.rs", "src/lib.rs"]);
    }

    #[test]
    fn test_extract_pathspecs_no_separator() {
        let argv = vec!["git".to_string(), "stash".to_string(), "push".to_string()];
        let specs = extract_pathspecs_from_argv(&argv);
        assert!(specs.is_empty());
    }

    #[test]
    fn test_save_with_pathspec_filters_entries() {
        let (_dir, repo_path) = setup_repo();
        let git_dir = resolve_git_dir(&repo_path).unwrap();

        // Create initial commit with two files
        let _base_sha = commit_file(&repo_path, "a.txt", "aaa\n", "init");
        std::fs::write(repo_path.join("b.txt"), "bbb\n").unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["add", "b.txt"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["commit", "-m", "add b"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        let base_sha2 = git_in_repo(&repo_path, &["rev-parse", "HEAD"]).unwrap();

        // Write working log entries for both files
        let entry_a = WorkingLogEntry {
            file: "a.txt".into(),
            blob_sha: "aaa".into(),
            attributions: vec![],
            line_attributions: vec![],
        };
        let entry_b = WorkingLogEntry {
            file: "b.txt".into(),
            blob_sha: "bbb".into(),
            attributions: vec![],
            line_attributions: vec![],
        };
        let checkpoint = Checkpoint::new(
            CheckpointKind::AiAgent,
            "claude".into(),
            vec![entry_a, entry_b],
        );
        working_log::append_checkpoint(&git_dir, &base_sha2, &checkpoint);

        // Modify both files and stash only a.txt
        std::fs::write(repo_path.join("a.txt"), "modified a\n").unwrap();
        std::fs::write(repo_path.join("b.txt"), "modified b\n").unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["stash", "push", "--", "a.txt"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        // Save with pathspec for a.txt only
        let pathspecs = vec!["a.txt".to_string()];
        let result = save_stash_attributions(&repo_path, &base_sha2, &pathspecs);
        assert!(result.is_ok(), "save failed: {:?}", result.err());

        // Verify that b.txt's entries remain in the working log
        let remaining = working_log::read_checkpoints(&git_dir, &base_sha2);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].entries.len(), 1);
        assert_eq!(remaining[0].entries[0].file, "b.txt");
    }
}
