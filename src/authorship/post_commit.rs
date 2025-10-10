use crate::authorship::authorship_log::LineRange;
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::stats::{stats_for_commit_stats, write_stats_to_terminal};
use crate::authorship::working_log::Checkpoint;
use crate::commands::checkpoint_agent::agent_preset::CursorPreset;
use crate::error::GitAiError;
use crate::git::refs::notes_add;
use crate::git::repository::Repository;
use crate::utils::{Timer, debug_log};
use std::collections::{HashMap, HashSet};

pub fn post_commit(
    repo: &Repository,
    base_commit: Option<String>,
    commit_sha: String,
    human_author: String,
    supress_output: bool,
) -> Result<(String, AuthorshipLog), GitAiError> {
    let mut timer = Timer::new();

    // Use base_commit parameter if provided, otherwise use "initial" for empty repos
    // This matches the convention in checkpoint.rs
    let parent_sha = base_commit.unwrap_or_else(|| "initial".to_string());

    // Initialize the new storage system
    let repo_storage = &repo.storage;
    timer.start("working_log_for_base_commit");
    let working_log = repo_storage.working_log_for_base_commit(&parent_sha);
    timer.end("working_log_for_base_commit");

    // Pull all working log entries from the parent commit

    let parent_working_log = working_log.read_all_checkpoints()?;

    debug_log(&format!(
        "edited files: {:?}",
        parent_working_log.edited_files
    ));

    // Filter out untracked files from the working log
    timer.start("filter_untracked_files");
    let mut filtered_working_log = filter_untracked_files(
        repo,
        &parent_working_log.checkpoints,
        &commit_sha,
        Some(&parent_working_log.edited_files),
    )?;
    timer.end("filter_untracked_files");

    // mutates inline
    CursorPreset::update_cursor_conversations_to_latest(&mut filtered_working_log)?;

    timer.start("compute_authorship_log");
    // --- NEW: Serialize authorship log and store it in notes/ai/{commit_sha} ---
    let mut authorship_log = AuthorshipLog::from_working_log_with_base_commit_and_human_author(
        &filtered_working_log,
        &parent_sha,
        Some(&human_author),
    );
    timer.end("compute_authorship_log");

    // Filter the authorship log to only include committed lines
    // We need to keep ONLY lines that are in the commit, not filter out unstaged lines
    timer.start("collect_committed_hunks");
    let committed_hunks = collect_committed_hunks(
        repo,
        &parent_sha,
        &commit_sha,
        Some(&parent_working_log.edited_files),
    )?;
    timer.end("collect_committed_hunks");

    // Convert authorship log line numbers from working directory coordinates to commit coordinates
    // The working log uses working directory coordinates (which includes unstaged changes),
    // but the authorship log should store commit coordinates (line numbers as they appear in the commit tree)
    timer.start("collect_unstaged_hunks");
    let unstaged_hunks =
        collect_unstaged_hunks(repo, &commit_sha, Some(&parent_working_log.edited_files))?;
    timer.end("collect_unstaged_hunks");

    // Convert working directory line numbers to commit line numbers
    timer.start("convert_authorship_log_to_commit_coordinates");
    convert_authorship_log_to_commit_coordinates(&mut authorship_log, &unstaged_hunks);
    timer.end("convert_authorship_log_to_commit_coordinates");

    timer.start("filter_to_committed_lines");
    // Now filter to only include committed lines
    authorship_log.filter_to_committed_lines(&committed_hunks);
    timer.end("filter_to_committed_lines");

    // Check if there are unstaged AI-authored lines to preserve in working log
    let has_unstaged_ai_lines = if !unstaged_hunks.is_empty() {
        // Check if any unstaged lines match the working log
        let parent_working_log = repo_storage.working_log_for_base_commit(&parent_sha);
        let parent_checkpoints = parent_working_log
            .read_all_checkpoints()
            .map(|wl| wl.checkpoints)
            .unwrap_or_default();

        !parent_checkpoints.is_empty() && parent_checkpoints.iter().any(|cp| cp.agent_id.is_some())
    } else {
        false
    };

    // Serialize the authorship log
    let authorship_json = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

    timer.start("notes_add");
    notes_add(repo, &commit_sha, &authorship_json)?;
    timer.end("notes_add");

    // Only delete the working log if there are no unstaged AI-authored lines
    // If there are unstaged AI lines, filter and transfer the working log to the new commit
    if !has_unstaged_ai_lines {
        if !cfg!(debug_assertions) {
            repo_storage.delete_working_log_for_base_commit(&parent_sha)?;
        }
    } else {
        // Filter the working log to remove committed lines, keeping only unstaged ones
        let parent_working_log = repo_storage.working_log_for_base_commit(&parent_sha);
        let parent_checkpoints = parent_working_log.read_all_checkpoints()?.checkpoints;

        let new_working_log = repo_storage.working_log_for_base_commit(&commit_sha);

        for mut checkpoint in parent_checkpoints {
            // Filter entries to only include unstaged lines
            for entry in &mut checkpoint.entries {
                if let Some(unstaged_ranges) = unstaged_hunks.get(&entry.file) {
                    // Expand all lines from added_lines, filter to unstaged, then recompress
                    let mut all_lines: Vec<u32> = Vec::new();
                    for line in &entry.added_lines {
                        match line {
                            crate::authorship::working_log::Line::Single(l) => all_lines.push(*l),
                            crate::authorship::working_log::Line::Range(start, end) => {
                                all_lines.extend(*start..=*end);
                            }
                        }
                    }

                    // Keep only unstaged lines
                    all_lines.retain(|l| unstaged_ranges.iter().any(|range| range.contains(*l)));

                    // Recompress to Line format
                    entry.added_lines = crate::authorship::authorship_log_serialization::compress_lines_to_working_log_format(&all_lines);

                    // Clear deleted_lines since they're relative to the old base commit
                    // After a commit, the base commit changes, so old deletions are no longer relevant
                    entry.deleted_lines.clear();
                }
            }

            // Remove entries with no lines left
            checkpoint
                .entries
                .retain(|entry| !entry.added_lines.is_empty());

            // Only append if there are entries left
            if !checkpoint.entries.is_empty() {
                new_working_log.append_checkpoint(&checkpoint)?;
            }
        }

        // Delete the old working log, but keep it in debug mode
        if !cfg!(debug_assertions) {
            repo_storage.delete_working_log_for_base_commit(&parent_sha)?;
        }
    }

    if !supress_output {
        let refname = repo.head()?.name().unwrap().to_string();
        let stats = stats_for_commit_stats(repo, &commit_sha, &refname)?;
        write_stats_to_terminal(&stats);
    }
    Ok((commit_sha.to_string(), authorship_log))
}

/// Filter out working log entries for untracked files
fn filter_untracked_files(
    repo: &Repository,
    working_log: &[Checkpoint],
    commit_sha: &str,
    pathspecs: Option<&HashSet<String>>,
) -> Result<Vec<Checkpoint>, GitAiError> {
    // Get all files changed in current commit in ONE git command (scoped to pathspecs)
    // If a file from the working log is in this set, it was committed. Otherwise, it was untracked.
    let committed_files = repo.list_commit_files(commit_sha, pathspecs)?;

    // Filter the working log to only include files that were actually committed
    let mut filtered_checkpoints = Vec::new();

    for checkpoint in working_log {
        let mut filtered_entries = Vec::new();

        for entry in &checkpoint.entries {
            // Keep entry only if this file was in the commit
            if committed_files.contains(&entry.file) {
                filtered_entries.push(entry.clone());
            }
        }

        // Only include checkpoints that have at least one committed file entry
        if !filtered_entries.is_empty() {
            let mut filtered_checkpoint = checkpoint.clone();
            filtered_checkpoint.entries = filtered_entries;
            filtered_checkpoints.push(filtered_checkpoint);
        }
    }

    Ok(filtered_checkpoints)
}

/// Collect line ranges that were committed (present in current commit but added from parent)
///
/// This function diffs the parent commit against the current commit to find all lines
/// that were added/changed in this commit. Only these lines should be in the authorship log.
fn collect_committed_hunks(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    pathspecs: Option<&HashSet<String>>,
) -> Result<HashMap<String, Vec<LineRange>>, GitAiError> {
    let mut committed_hunks: HashMap<String, Vec<LineRange>> = HashMap::new();

    // Handle initial commit (no parent)
    if parent_sha == "initial" {
        // For initial commit, use git diff against the empty tree
        let empty_tree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"; // Git's empty tree hash
        let added_lines = repo.diff_added_lines(empty_tree, commit_sha, pathspecs)?;

        for (file_path, lines) in added_lines {
            if !lines.is_empty() {
                committed_hunks.insert(file_path, LineRange::compress_lines(&lines));
            }
        }

        return Ok(committed_hunks);
    }

    // Use git diff to get added lines directly
    let added_lines = repo.diff_added_lines(parent_sha, commit_sha, pathspecs)?;

    for (file_path, lines) in added_lines {
        if !lines.is_empty() {
            committed_hunks.insert(file_path, LineRange::compress_lines(&lines));
        }
    }

    Ok(committed_hunks)
}

/// Collect all unstaged line ranges from the working directory
///
/// This function diffs the HEAD commit (what was just committed) against the working directory
/// to find all lines that exist in the working directory but weren't part of the commit.
/// These lines should be excluded from the authorship log.
fn collect_unstaged_hunks(
    repo: &Repository,
    commit_sha: &str,
    pathspecs: Option<&HashSet<String>>,
) -> Result<HashMap<String, Vec<LineRange>>, GitAiError> {
    let mut unstaged_hunks: HashMap<String, Vec<LineRange>> = HashMap::new();

    // Use git diff to get added lines in working directory vs commit
    let added_lines = repo.diff_workdir_added_lines(commit_sha, pathspecs)?;

    for (file_path, lines) in added_lines {
        if !lines.is_empty() {
            unstaged_hunks.insert(file_path, LineRange::compress_lines(&lines));
        }
    }

    Ok(unstaged_hunks)
}

/// Convert authorship log line numbers from working directory coordinates to commit coordinates
///
/// The working log records line numbers in working directory coordinates (which includes unstaged changes),
/// but the authorship log should store commit coordinates (line numbers as they appear in the commit tree).
/// This function adjusts all line numbers in the authorship log by subtracting the number of unstaged lines
/// above each line.
///
/// For example, if there's an unstaged line at position 1, then working directory line 22 becomes commit line 21,
/// and working directory line 31 becomes commit line 30.
fn convert_authorship_log_to_commit_coordinates(
    authorship_log: &mut AuthorshipLog,
    unstaged_hunks: &HashMap<String, Vec<LineRange>>,
) {
    for file_attestation in &mut authorship_log.attestations {
        if let Some(unstaged_ranges) = unstaged_hunks.get(&file_attestation.file_path) {
            // Expand unstaged ranges to individual line numbers for easier comparison
            let mut unstaged_lines: Vec<u32> = Vec::new();
            for range in unstaged_ranges {
                unstaged_lines.extend(range.expand());
            }
            unstaged_lines.sort_unstable();

            // For each attestation entry, convert working directory line numbers to commit line numbers
            for entry in &mut file_attestation.entries {
                // Expand entry's line ranges to individual lines
                let mut entry_lines: Vec<u32> = Vec::new();
                for range in &entry.line_ranges {
                    entry_lines.extend(range.expand());
                }

                // Convert each line from working directory coordinates to commit coordinates
                let mut converted_lines: Vec<u32> = Vec::new();
                for workdir_line in entry_lines {
                    // Count how many unstaged lines are strictly before this line
                    let adjustment =
                        unstaged_lines.iter().filter(|&&l| l < workdir_line).count() as u32;
                    let commit_line = workdir_line - adjustment;
                    converted_lines.push(commit_line);
                }

                if !converted_lines.is_empty() {
                    converted_lines.sort_unstable();
                    converted_lines.dedup();
                    entry.line_ranges = LineRange::compress_lines(&converted_lines);
                } else {
                    entry.line_ranges.clear();
                }
            }

            // Remove entries that have no line ranges left
            file_attestation
                .entries
                .retain(|entry| !entry.line_ranges.is_empty());
        }
    }

    // Remove file attestations that have no entries left
    authorship_log
        .attestations
        .retain(|file| !file.entries.is_empty());
}

#[cfg(test)]
mod tests {
    use crate::git::test_utils::TmpRepo;

    #[test]
    fn test_post_commit_empty_repo_with_checkpoint() {
        // Create an empty repo (no commits yet)
        let tmp_repo = TmpRepo::new().unwrap();

        // Create a file and checkpoint it (no commit yet)
        let mut file = tmp_repo
            .write_file("test.txt", "Hello, world!\n", false)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();

        // Make a change and checkpoint again
        file.append("Second line\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();

        // Now make the first commit (empty repo case: base_commit is None)
        let result = tmp_repo.commit_with_message("Initial commit");

        // Should not panic or error - this is the key test
        // The main goal is to ensure empty repos (base_commit=None) don't cause errors
        assert!(
            result.is_ok(),
            "post_commit should handle empty repo (base_commit=None) without errors"
        );

        // The authorship log is created successfully (even if empty for human-only checkpoints)
        let _authorship_log = result.unwrap();
    }

    #[test]
    fn test_post_commit_empty_repo_no_checkpoint() {
        // Create an empty repo (no commits yet)
        let tmp_repo = TmpRepo::new().unwrap();

        // Create a file without checkpointing
        tmp_repo
            .write_file("test.txt", "Hello, world!\n", false)
            .unwrap();

        // Make the first commit with no prior checkpoints
        let result = tmp_repo.commit_with_message("Initial commit");

        // Should not panic or error even with no working log
        assert!(
            result.is_ok(),
            "post_commit should handle empty repo with no checkpoints without errors"
        );

        let authorship_log = result.unwrap();

        // The authorship log should be created but empty (no AI checkpoints)
        // All changes will be attributed to the human author
        assert!(
            authorship_log.attestations.is_empty(),
            "Should have empty attestations when no checkpoints exist"
        );
    }
}
