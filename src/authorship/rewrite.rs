use std::collections::HashMap;

use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::hunk_shift::{apply_hunk_shifts_to_file_attestation, parse_hunk_header, DiffHunk};
use crate::error::GitAiError;
use crate::git::repository::{exec_git, exec_git_allow_nonzero, Repository};

pub enum RewriteEvent {
    NonFastForward { old_tip: String, new_tip: String },
    CherryPickComplete { sources: Vec<String>, new_commits: Vec<String> },
}

pub(crate) struct DiffTreeResult {
    pub hunks_by_file: HashMap<String, Vec<DiffHunk>>,
    pub renames: Vec<(String, String)>,
}

pub fn handle_rewrite_event(repo: &Repository, event: RewriteEvent) -> Result<(), GitAiError> {
    let mappings = match event {
        RewriteEvent::NonFastForward { old_tip, new_tip } => {
            derive_mappings_from_range_diff(repo, &old_tip, &new_tip)?
        }
        RewriteEvent::CherryPickComplete { sources, new_commits } => {
            sources.into_iter().zip(new_commits).collect()
        }
    };
    if mappings.is_empty() {
        return Ok(());
    }
    shift_authorship_notes(repo, &mappings)?;
    migrate_working_log_if_needed(repo, &mappings)?;
    Ok(())
}

pub fn shift_authorship_notes(
    repo: &Repository,
    mappings: &[(String, String)],
) -> Result<(), GitAiError> {
    let mut notes_to_write: Vec<(String, String)> = Vec::new();

    for (source_sha, new_sha) in mappings {
        let Some(raw_note) = read_authorship_note(repo, source_sha)? else {
            continue;
        };

        let Ok(mut log) = AuthorshipLog::deserialize_from_string(&raw_note) else {
            notes_to_write.push((new_sha.clone(), raw_note));
            continue;
        };

        let diff_result = match compute_diff_tree(repo, source_sha, new_sha) {
            Ok(r) => r,
            Err(_) => {
                notes_to_write.push((new_sha.clone(), raw_note));
                continue;
            }
        };

        // Apply renames
        for (old_path, new_path) in &diff_result.renames {
            for attestation in &mut log.attestations {
                if attestation.file_path == *old_path {
                    attestation.file_path = new_path.clone();
                }
            }
        }

        // Shift attestations
        let shifted: Vec<_> = log
            .attestations
            .iter()
            .filter_map(|fa| {
                let hunks = diff_result.hunks_by_file.get(&fa.file_path);
                match hunks {
                    Some(h) if !h.is_empty() => apply_hunk_shifts_to_file_attestation(fa, h),
                    _ => Some(fa.clone()),
                }
            })
            .collect();
        log.attestations = shifted;

        log.metadata.base_commit_sha = new_sha.clone();

        match log.serialize_to_string() {
            Ok(serialized) => notes_to_write.push((new_sha.clone(), serialized)),
            Err(_) => notes_to_write.push((new_sha.clone(), raw_note)),
        }
    }

    for (sha, content) in &notes_to_write {
        write_authorship_note(repo, sha, content)?;
    }

    Ok(())
}

pub fn derive_mappings_from_range_diff(
    _repo: &Repository,
    _old_tip: &str,
    _new_tip: &str,
) -> Result<Vec<(String, String)>, GitAiError> {
    Ok(Vec::new()) // Task 3
}

pub fn migrate_working_log_if_needed(
    _repo: &Repository,
    _mappings: &[(String, String)],
) -> Result<(), GitAiError> {
    Ok(()) // Task 5
}

fn compute_diff_tree(
    repo: &Repository,
    source_sha: &str,
    new_sha: &str,
) -> Result<DiffTreeResult, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "diff-tree".to_string(),
        "-p".to_string(),
        "-U0".to_string(),
        "-M".to_string(),
        "--no-color".to_string(),
        source_sha.to_string(),
        new_sha.to_string(),
    ]);

    let output = exec_git_allow_nonzero(&args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_diff_tree_output(&stdout))
}

fn parse_diff_tree_output(output: &str) -> DiffTreeResult {
    let mut hunks_by_file: HashMap<String, Vec<DiffHunk>> = HashMap::new();
    let mut renames: Vec<(String, String)> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_rename_from: Option<String> = None;

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // Extract the b/ path from "a/old b/new"
            current_file = extract_b_path(rest);
            current_rename_from = None;
        } else if let Some(from_path) = line.strip_prefix("rename from ") {
            current_rename_from = Some(from_path.to_string());
        } else if let Some(to_path) = line.strip_prefix("rename to ") {
            if let Some(from_path) = current_rename_from.take() {
                renames.push((from_path, to_path.to_string()));
            }
        } else if line.starts_with("@@") {
            if let Some(ref file) = current_file {
                if let Some(hunk) = parse_hunk_header(line) {
                    hunks_by_file.entry(file.clone()).or_default().push(hunk);
                }
            }
        }
    }

    DiffTreeResult { hunks_by_file, renames }
}

fn extract_b_path(diff_header: &str) -> Option<String> {
    // Format: "a/path b/path" or "a/path with spaces b/path with spaces"
    // The b/ path starts after the last occurrence of " b/"
    let marker = " b/";
    let pos = diff_header.rfind(marker)?;
    Some(diff_header[pos + marker.len()..].to_string())
}

fn read_authorship_note(repo: &Repository, sha: &str) -> Result<Option<String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "notes".to_string(),
        "--ref=ai".to_string(),
        "show".to_string(),
        sha.to_string(),
    ]);

    let output = exec_git_allow_nonzero(&args)?;
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
    } else {
        Ok(None)
    }
}

fn write_authorship_note(repo: &Repository, sha: &str, content: &str) -> Result<(), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "notes".to_string(),
        "--ref=ai".to_string(),
        "add".to_string(),
        "-f".to_string(),
        "-m".to_string(),
        content.to_string(),
        sha.to_string(),
    ]);

    exec_git(&args)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_b_path_simple() {
        assert_eq!(extract_b_path("a/src/main.rs b/src/main.rs"), Some("src/main.rs".to_string()));
    }

    #[test]
    fn test_extract_b_path_rename() {
        assert_eq!(
            extract_b_path("a/src/old.rs b/src/new.rs"),
            Some("src/new.rs".to_string())
        );
    }

    #[test]
    fn test_extract_b_path_with_spaces() {
        assert_eq!(
            extract_b_path("a/path with spaces b/another path"),
            Some("another path".to_string())
        );
    }

    #[test]
    fn test_parse_diff_tree_output_simple() {
        let output = "\
diff --git a/src/foo.rs b/src/foo.rs
index abc123..def456 100644
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -10,3 +10,5 @@ fn foo()
+added line 1
+added line 2
";
        let result = parse_diff_tree_output(output);
        assert!(result.renames.is_empty());
        assert_eq!(result.hunks_by_file.len(), 1);
        let hunks = &result.hunks_by_file["src/foo.rs"];
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 10);
        assert_eq!(hunks[0].old_count, 3);
        assert_eq!(hunks[0].new_start, 10);
        assert_eq!(hunks[0].new_count, 5);
    }

    #[test]
    fn test_parse_diff_tree_output_with_rename() {
        let output = "\
diff --git a/src/old.rs b/src/new.rs
similarity index 90%
rename from src/old.rs
rename to src/new.rs
index abc123..def456 100644
--- a/src/old.rs
+++ b/src/new.rs
@@ -5,2 +5,3 @@ fn bar()
+new line
";
        let result = parse_diff_tree_output(output);
        assert_eq!(result.renames.len(), 1);
        assert_eq!(result.renames[0], ("src/old.rs".to_string(), "src/new.rs".to_string()));
        let hunks = &result.hunks_by_file["src/new.rs"];
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 5);
        assert_eq!(hunks[0].old_count, 2);
        assert_eq!(hunks[0].new_start, 5);
        assert_eq!(hunks[0].new_count, 3);
    }

    #[test]
    fn test_parse_diff_tree_output_multiple_files() {
        let output = "\
diff --git a/file1.rs b/file1.rs
index aaa..bbb 100644
--- a/file1.rs
+++ b/file1.rs
@@ -1,2 +1,3 @@
+line
diff --git a/file2.rs b/file2.rs
index ccc..ddd 100644
--- a/file2.rs
+++ b/file2.rs
@@ -10,0 +11,2 @@
+line1
+line2
";
        let result = parse_diff_tree_output(output);
        assert_eq!(result.hunks_by_file.len(), 2);
        assert_eq!(result.hunks_by_file["file1.rs"].len(), 1);
        assert_eq!(result.hunks_by_file["file2.rs"].len(), 1);
        assert_eq!(result.hunks_by_file["file2.rs"][0].old_start, 10);
        assert_eq!(result.hunks_by_file["file2.rs"][0].old_count, 0);
        assert_eq!(result.hunks_by_file["file2.rs"][0].new_start, 11);
        assert_eq!(result.hunks_by_file["file2.rs"][0].new_count, 2);
    }

    #[test]
    fn test_parse_diff_tree_output_binary() {
        let output = "\
diff --git a/image.png b/image.png
Binary files a/image.png and b/image.png differ
";
        let result = parse_diff_tree_output(output);
        // No hunks for binary files
        assert!(
            result.hunks_by_file.get("image.png").map_or(true, |h| h.is_empty())
        );
    }

    #[test]
    fn test_parse_diff_tree_empty_output() {
        let result = parse_diff_tree_output("");
        assert!(result.hunks_by_file.is_empty());
        assert!(result.renames.is_empty());
    }
}
