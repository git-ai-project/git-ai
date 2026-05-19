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
    repo: &Repository,
    old_tip: &str,
    new_tip: &str,
) -> Result<Vec<(String, String)>, GitAiError> {
    let Some(base) = find_merge_base(repo, old_tip, new_tip) else {
        return Ok(Vec::new());
    };

    // Rewind: branch moved backward
    if base == new_tip {
        return Ok(Vec::new());
    }

    // Fast-forward: no rewrite happened
    if base == old_tip {
        return Ok(Vec::new());
    }

    // Full squash: all old commits collapsed into one new commit
    if is_full_squash(repo, &base, old_tip, new_tip) {
        return Ok(vec![(old_tip.to_string(), new_tip.to_string())]);
    }

    let range_diff_output = run_range_diff(repo, &base, old_tip, new_tip)?;
    let mut mappings = parse_range_diff_output(&range_diff_output);

    let merge_mappings = derive_merge_commit_mappings(repo, &base, old_tip, new_tip, &mappings)?;
    mappings.extend(merge_mappings);

    Ok(mappings)
}

fn find_merge_base(repo: &Repository, a: &str, b: &str) -> Option<String> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "merge-base".to_string(),
        a.to_string(),
        b.to_string(),
    ]);

    let output = exec_git_allow_nonzero(&args).ok()?;
    if !output.status.success() {
        return None;
    }
    let base = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if base.is_empty() {
        None
    } else {
        Some(base)
    }
}

fn is_full_squash(repo: &Repository, base: &str, old_tip: &str, new_tip: &str) -> bool {
    // Check new_tip^ == base (exactly one commit between base and new_tip)
    let mut args = repo.global_args_for_exec();
    args.extend([
        "rev-parse".to_string(),
        format!("{}^", new_tip),
    ]);
    let Ok(output) = exec_git_allow_nonzero(&args) else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let parent = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if parent != base {
        return false;
    }

    // Check new_tip is not a merge commit (new_tip^2 should fail)
    let mut args = repo.global_args_for_exec();
    args.extend([
        "rev-parse".to_string(),
        format!("{}^2", new_tip),
    ]);
    let Ok(output) = exec_git_allow_nonzero(&args) else {
        return false;
    };
    if output.status.success() {
        return false;
    }

    // Check multiple old commits existed
    let mut args = repo.global_args_for_exec();
    args.extend([
        "rev-list".to_string(),
        "--count".to_string(),
        format!("{}..{}", base, old_tip),
    ]);
    let Ok(output) = exec_git_allow_nonzero(&args) else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let count: usize = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    count > 1
}

fn run_range_diff(
    repo: &Repository,
    base: &str,
    old_tip: &str,
    new_tip: &str,
) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "range-diff".to_string(),
        "--no-color".to_string(),
        "--no-abbrev".to_string(),
        "-s".to_string(),
        "--creation-factor=100".to_string(),
        format!("{}..{}", base, old_tip),
        format!("{}..{}", base, new_tip),
    ]);
    let output = exec_git(&args)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_range_diff_output(output: &str) -> Vec<(String, String)> {
    let mut mappings = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Find first 40-char hex SHA
        let Some((old_sha, rest)) = find_next_sha(trimmed) else {
            continue;
        };

        // Skip whitespace, read status character
        let rest = rest.trim_start();
        let Some(status_char) = rest.chars().next() else {
            continue;
        };

        // Only matched pairs (= or !) are useful
        if status_char != '=' && status_char != '!' {
            continue;
        }

        // Find second 40-char hex SHA
        let after_status = &rest[status_char.len_utf8()..];
        let Some((new_sha, _)) = find_next_sha(after_status) else {
            continue;
        };

        // Skip null SHAs
        if old_sha.chars().all(|c| c == '0') || new_sha.chars().all(|c| c == '0') {
            continue;
        }

        mappings.push((old_sha, new_sha));
    }

    mappings
}

fn find_next_sha(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 40 <= bytes.len() {
        let candidate = &s[i..i + 40];
        if is_hex_sha(candidate) {
            return Some((candidate.to_string(), &s[i + 40..]));
        }
        i += 1;
    }
    None
}

fn is_hex_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn derive_merge_commit_mappings(
    repo: &Repository,
    base: &str,
    old_tip: &str,
    new_tip: &str,
    existing_mappings: &[(String, String)],
) -> Result<Vec<(String, String)>, GitAiError> {
    let old_merges = list_merge_commits(repo, base, old_tip)?;
    let new_merges = list_merge_commits(repo, base, new_tip)?;

    let mut merge_mappings: Vec<(String, String)> = Vec::new();

    for old_merge in &old_merges {
        // Only map merges that have authorship notes
        let has_note = read_authorship_note(repo, old_merge)?.is_some();
        if !has_note {
            continue;
        }

        let old_parents = get_commit_parents(repo, old_merge);
        if old_parents.is_empty() {
            continue;
        }

        // For each new merge, check if its parents are the mapped equivalents of old_merge's parents
        for new_merge in &new_merges {
            // Skip if already used in a mapping
            if merge_mappings.iter().any(|(_, n)| n == new_merge) {
                continue;
            }

            let new_parents = get_commit_parents(repo, new_merge);
            if new_parents.len() != old_parents.len() {
                continue;
            }

            let all_match = old_parents.iter().zip(new_parents.iter()).all(|(op, np)| {
                // Check in existing_mappings
                if existing_mappings.iter().any(|(o, n)| o == op && n == np) {
                    return true;
                }
                // Check in already-matched merge_mappings
                if merge_mappings.iter().any(|(o, n)| o == op && n == np) {
                    return true;
                }
                // Unmapped parent that stayed the same (e.g., shared ancestor)
                op == np
            });

            if all_match {
                merge_mappings.push((old_merge.clone(), new_merge.clone()));
                break;
            }
        }
    }

    Ok(merge_mappings)
}

fn list_merge_commits(repo: &Repository, base: &str, tip: &str) -> Result<Vec<String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "rev-list".to_string(),
        "--merges".to_string(),
        "--topo-order".to_string(),
        "--reverse".to_string(),
        format!("{}..{}", base, tip),
    ]);

    let output = exec_git_allow_nonzero(&args)?;
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

fn get_commit_parents(repo: &Repository, sha: &str) -> Vec<String> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "rev-parse".to_string(),
        format!("{}^@", sha),
    ]);

    let Ok(output) = exec_git_allow_nonzero(&args) else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
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

    #[test]
    fn test_is_hex_sha_valid() {
        assert!(is_hex_sha("a" .repeat(40).as_str()));
        assert!(is_hex_sha("0123456789abcdef0123456789abcdef01234567"));
        assert!(is_hex_sha("ABCDEF0123456789abcdef0123456789abcdef01"));
    }

    #[test]
    fn test_is_hex_sha_invalid() {
        assert!(!is_hex_sha("short"));
        assert!(!is_hex_sha("g123456789abcdef0123456789abcdef01234567"));
        assert!(!is_hex_sha("0123456789abcdef0123456789abcdef0123456"));  // 39 chars
        assert!(!is_hex_sha("0123456789abcdef0123456789abcdef012345678")); // 41 chars
        assert!(!is_hex_sha(""));
    }

    #[test]
    fn test_parse_range_diff_output_matched_equal() {
        let output = " 1:  aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa = 1:  bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb Some commit subject\n";
        let mappings = parse_range_diff_output(output);
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(mappings[0].1, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    }

    #[test]
    fn test_parse_range_diff_output_matched_bang() {
        let output = " 2:  1111111111111111111111111111111111111111 ! 3:  2222222222222222222222222222222222222222 Modified commit\n";
        let mappings = parse_range_diff_output(output);
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, "1111111111111111111111111111111111111111");
        assert_eq!(mappings[0].1, "2222222222222222222222222222222222222222");
    }

    #[test]
    fn test_parse_range_diff_output_dropped_and_new() {
        let output = "\
 1:  aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa < -:  0000000000000000000000000000000000000000 Dropped commit
 -:  0000000000000000000000000000000000000000 > 1:  cccccccccccccccccccccccccccccccccccccccc New commit
";
        let mappings = parse_range_diff_output(output);
        assert!(mappings.is_empty());
    }

    #[test]
    fn test_parse_range_diff_output_null_shas_skipped() {
        let output = " 1:  0000000000000000000000000000000000000000 = 1:  bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb Subject\n";
        let mappings = parse_range_diff_output(output);
        assert!(mappings.is_empty());
    }

    #[test]
    fn test_parse_range_diff_output_multiple_lines() {
        let output = "\
 1:  aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa = 1:  bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb First commit
 2:  cccccccccccccccccccccccccccccccccccccccc ! 2:  dddddddddddddddddddddddddddddddddddddddd Second commit
 3:  eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee = 3:  ffffffffffffffffffffffffffffffffffffffff Third commit
";
        let mappings = parse_range_diff_output(output);
        assert_eq!(mappings.len(), 3);
        assert_eq!(mappings[0], ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()));
        assert_eq!(mappings[1], ("cccccccccccccccccccccccccccccccccccccccc".to_string(), "dddddddddddddddddddddddddddddddddddddddd".to_string()));
        assert_eq!(mappings[2], ("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string(), "ffffffffffffffffffffffffffffffffffffffff".to_string()));
    }

    #[test]
    fn test_parse_range_diff_output_empty() {
        let mappings = parse_range_diff_output("");
        assert!(mappings.is_empty());
    }
}
