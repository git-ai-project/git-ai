//! Merge conflict attribution tracking.
//!
//! After `git merge` completes (with or without conflicts), this module
//! tracks attribution for the merge commit by combining authorship notes
//! from both parents and handling conflict resolution lines.
//!
//! For non-conflicting merges: the merge commit inherits attribution from
//! both parents (union of notes).
//!
//! For conflicting merges that were resolved:
//! - Lines that match one parent -> attribute to that parent's original attribution
//! - Lines that are NEW (not from either parent) -> attribute as untracked (human resolved)

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

use super::authorship_log::{
    AttestationEntry, AuthorshipLog, FileAttestation, LineRange, Metadata,
};

// ---------------------------------------------------------------------------
// Git helpers
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

/// Read a git note for a commit from the ai namespace.
fn read_authorship_note(repo_path: &Path, commit_sha: &str) -> Option<AuthorshipLog> {
    let note_content = git_in_repo(repo_path, &["notes", "--ref=ai", "show", commit_sha]).ok()?;
    AuthorshipLog::deserialize_from_string(&note_content).ok()
}

/// Get the file content at a specific revision.
fn git_show_file(repo_path: &Path, revision: &str, file_path: &str) -> Option<String> {
    git_in_repo(repo_path, &["show", &format!("{}:{}", revision, file_path)]).ok()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compute and write merge attribution for a merge commit.
///
/// This function:
/// 1. Reads the parent SHAs of the merge commit
/// 2. For each parent, reads its authorship note
/// 3. Combines attributions from both parents for the merge commit
/// 4. For files that differ between parents and the merge, determines
///    which lines came from which parent
/// 5. Writes a combined authorship note for the merge commit
///
/// Returns Ok(()) on success, or an error string.
/// This is best-effort: if parent notes don't exist, they are skipped gracefully.
pub fn compute_merge_attribution(repo_path: &Path, merge_commit: &str) -> Result<(), String> {
    // Check if a note already exists for this merge commit
    if git_in_repo(repo_path, &["notes", "--ref=ai", "show", merge_commit]).is_ok() {
        return Ok(()); // Already annotated
    }

    // Get parent SHAs
    let parents_str = git_in_repo(repo_path, &["log", "--format=%P", "-1", merge_commit])?;
    let parents: Vec<&str> = parents_str.split_whitespace().collect();

    if parents.len() < 2 {
        return Err("not a merge commit (fewer than 2 parents)".to_string());
    }

    // Read authorship notes from all parents
    let parent_notes: Vec<(String, AuthorshipLog)> = parents
        .iter()
        .filter_map(|&sha| read_authorship_note(repo_path, sha).map(|log| (sha.to_string(), log)))
        .collect();

    if parent_notes.is_empty() {
        // No parent notes exist — nothing to combine
        return Ok(());
    }

    // Determine which files changed in this merge
    let changed_files = get_merge_changed_files(repo_path, merge_commit, &parents)?;

    // Build combined attributions
    let combined_attestations = build_merge_attestations(
        repo_path,
        merge_commit,
        &parents,
        &parent_notes,
        &changed_files,
    );

    if combined_attestations.is_empty() {
        return Ok(());
    }

    // Build metadata (combine from all parents)
    let metadata = combine_metadata(merge_commit, &parent_notes);

    let log = AuthorshipLog {
        attestations: combined_attestations,
        metadata,
    };

    // Write the note
    let note_text = log.serialize_to_string();
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args([
            "notes",
            "--ref=ai",
            "add",
            "-f",
            "-m",
            &note_text,
            merge_commit,
        ])
        .env("GIT_TRACE2_EVENT", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .map_err(|e| format!("failed to run git notes: {}", e))?;

    if !status.success() {
        return Err(format!(
            "git notes add failed for merge {}",
            &merge_commit[..7.min(merge_commit.len())]
        ));
    }

    eprintln!(
        "[git-ai] merge: wrote combined authorship note for {}",
        &merge_commit[..7.min(merge_commit.len())]
    );

    Ok(())
}

/// Check if a commit is a merge commit (has more than one parent).
pub fn is_merge_commit(repo_path: &Path, commit_sha: &str) -> bool {
    if let Ok(parents_str) = git_in_repo(repo_path, &["log", "--format=%P", "-1", commit_sha]) {
        parents_str.split_whitespace().count() >= 2
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Internal: determine changed files
// ---------------------------------------------------------------------------

/// Get list of files that changed in the merge commit relative to any parent.
fn get_merge_changed_files(
    repo_path: &Path,
    merge_commit: &str,
    parents: &[&str],
) -> Result<Vec<String>, String> {
    let mut changed = Vec::new();

    for parent in parents {
        let output = git_in_repo(repo_path, &["diff", "--name-only", parent, merge_commit])?;
        for line in output.lines() {
            let file = line.trim().to_string();
            if !file.is_empty() && !changed.contains(&file) {
                changed.push(file);
            }
        }
    }

    Ok(changed)
}

// ---------------------------------------------------------------------------
// Internal: build merge attestations
// ---------------------------------------------------------------------------

/// Build attestation entries for the merge commit by combining parent notes.
fn build_merge_attestations(
    repo_path: &Path,
    merge_commit: &str,
    parents: &[&str],
    parent_notes: &[(String, AuthorshipLog)],
    changed_files: &[String],
) -> Vec<FileAttestation> {
    let mut result: Vec<FileAttestation> = Vec::new();

    // For each changed file, determine attribution
    for file_path in changed_files {
        let merge_content = match git_show_file(repo_path, merge_commit, file_path) {
            Some(c) => c,
            None => continue, // File was deleted in merge
        };

        let merge_lines: Vec<&str> = merge_content.lines().collect();
        if merge_lines.is_empty() {
            continue;
        }

        // Get file content and attestation from each parent
        let parent_data: Vec<(Option<String>, Option<&FileAttestation>)> = parents
            .iter()
            .map(|&parent_sha| {
                let content = git_show_file(repo_path, parent_sha, file_path);
                let attestation = parent_notes.iter().find_map(|(sha, log)| {
                    if sha == parent_sha {
                        log.attestations
                            .iter()
                            .find(|fa| fa.file_path == *file_path)
                    } else {
                        None
                    }
                });
                (content, attestation)
            })
            .collect();

        // For each line in the merge result, determine which parent it came from
        let mut line_sources: Vec<Option<usize>> = vec![None; merge_lines.len()];

        for (parent_idx, (content_opt, _)) in parent_data.iter().enumerate() {
            if let Some(content) = content_opt {
                let parent_lines: Vec<&str> = content.lines().collect();
                let mapping = compute_line_mapping_forward(&parent_lines, &merge_lines);
                for (_, &merge_line_idx) in mapping.iter() {
                    if merge_line_idx < line_sources.len() && line_sources[merge_line_idx].is_none()
                    {
                        line_sources[merge_line_idx] = Some(parent_idx);
                    }
                }
            }
        }

        // Build attestation entries based on line sources
        // Lines from a parent inherit that parent's attribution
        // Lines from no parent are "human resolved" (untracked)
        let mut entries_map: HashMap<String, Vec<u32>> = HashMap::new();

        for (line_idx, source) in line_sources.iter().enumerate() {
            let line_num = (line_idx + 1) as u32;

            if let Some(parent_idx) = source {
                // Line came from this parent — look up its attribution
                if let Some((_, attestation)) = parent_data.get(*parent_idx)
                    && let Some(file_att) = attestation
                {
                    // Find which author owns this line in the parent
                    if let Some(author) = find_line_author(
                        file_att,
                        line_num,
                        &parent_data,
                        *parent_idx,
                        &merge_lines,
                    ) {
                        entries_map.entry(author).or_default().push(line_num);
                        continue;
                    }
                }
                // Parent exists but has no attribution for this line — skip (untracked)
            }
            // else: line is from no parent (human resolution) — left untracked
        }

        // Convert to attestation entries
        if !entries_map.is_empty() {
            let mut entries: Vec<AttestationEntry> = Vec::new();
            for (hash, mut lines) in entries_map {
                lines.sort_unstable();
                lines.dedup();
                let ranges = LineRange::compress_lines(&lines);
                entries.push(AttestationEntry {
                    hash,
                    line_ranges: ranges,
                });
            }
            result.push(FileAttestation {
                file_path: file_path.clone(),
                entries,
            });
        }
    }

    result
}

/// Find the author of a line in a parent's attestation.
///
/// The `line_num` is in the MERGE commit's space. We need to map it back
/// to the parent's line space to look up the attestation.
fn find_line_author(
    file_attestation: &FileAttestation,
    merge_line_num: u32,
    parent_data: &[(Option<String>, Option<&FileAttestation>)],
    parent_idx: usize,
    merge_lines: &[&str],
) -> Option<String> {
    // Get parent content to compute reverse mapping
    let parent_content = parent_data.get(parent_idx)?.0.as_ref()?;
    let parent_lines: Vec<&str> = parent_content.lines().collect();

    // Map from merge line to parent line
    let mapping = compute_line_mapping_forward(&parent_lines, merge_lines);
    // Build reverse: merge_line_idx -> parent_line_idx
    let reverse: HashMap<usize, usize> = mapping
        .iter()
        .map(|(&parent_idx, &merge_idx)| (merge_idx, parent_idx))
        .collect();

    let merge_idx = (merge_line_num - 1) as usize;
    let parent_line_idx = reverse.get(&merge_idx)?;
    let parent_line_num = (*parent_line_idx + 1) as u32;

    // Look up which author owns this line in the parent's attestation
    for entry in &file_attestation.entries {
        for range in &entry.line_ranges {
            if range.contains(parent_line_num) {
                return Some(entry.hash.clone());
            }
        }
    }

    None
}

/// Compute a forward mapping from parent line indices to merge line indices.
/// Uses LCS (longest common subsequence) to find matching lines.
/// Returns a HashMap where key=parent_line_index, value=merge_line_index (0-based).
fn compute_line_mapping_forward(
    parent_lines: &[&str],
    merge_lines: &[&str],
) -> HashMap<usize, usize> {
    let m = parent_lines.len();
    let n = merge_lines.len();

    if m == 0 || n == 0 {
        return HashMap::new();
    }

    // Build DP table for LCS
    let mut dp = vec![vec![0u32; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            if parent_lines[i - 1] == merge_lines[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    // Backtrack to find pairs
    let mut mapping = HashMap::new();
    let mut i = m;
    let mut j = n;
    while i > 0 && j > 0 {
        if parent_lines[i - 1] == merge_lines[j - 1] {
            mapping.insert(i - 1, j - 1);
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }

    mapping
}

/// Combine metadata from all parent notes.
fn combine_metadata(merge_commit: &str, parent_notes: &[(String, AuthorshipLog)]) -> Metadata {
    use super::authorship_log::{self, HumanRecord, PromptRecord, SessionRecord};
    use std::collections::BTreeMap;

    let mut prompts: BTreeMap<String, PromptRecord> = BTreeMap::new();
    let mut sessions: BTreeMap<String, SessionRecord> = BTreeMap::new();
    let mut humans: BTreeMap<String, HumanRecord> = BTreeMap::new();

    for (_, log) in parent_notes {
        for (id, record) in &log.metadata.prompts {
            prompts.entry(id.clone()).or_insert_with(|| record.clone());
        }
        for (id, record) in &log.metadata.sessions {
            sessions.entry(id.clone()).or_insert_with(|| record.clone());
        }
        for (id, record) in &log.metadata.humans {
            humans.entry(id.clone()).or_insert_with(|| record.clone());
        }
    }

    Metadata {
        schema_version: authorship_log::AUTHORSHIP_LOG_VERSION.to_string(),
        git_ai_version: Some(authorship_log::GIT_AI_VERSION.to_string()),
        base_commit_sha: merge_commit.to_string(),
        prompts,
        sessions,
        humans,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::authorship_log::{AttestationEntry, FileAttestation, LineRange};

    #[test]
    fn test_compute_line_mapping_forward_basic() {
        let parent = vec!["a", "b", "c", "d"];
        let merge = vec!["a", "x", "b", "c", "d"];

        let mapping = compute_line_mapping_forward(&parent, &merge);

        // parent[0]="a" -> merge[0]="a"
        assert_eq!(mapping.get(&0), Some(&0));
        // parent[1]="b" -> merge[2]="b"
        assert_eq!(mapping.get(&1), Some(&2));
        // parent[2]="c" -> merge[3]="c"
        assert_eq!(mapping.get(&2), Some(&3));
        // parent[3]="d" -> merge[4]="d"
        assert_eq!(mapping.get(&3), Some(&4));
    }

    #[test]
    fn test_compute_line_mapping_forward_empty() {
        let parent: Vec<&str> = vec![];
        let merge = vec!["a", "b"];

        let mapping = compute_line_mapping_forward(&parent, &merge);
        assert!(mapping.is_empty());
    }

    #[test]
    fn test_compute_line_mapping_forward_no_overlap() {
        let parent = vec!["a", "b"];
        let merge = vec!["x", "y"];

        let mapping = compute_line_mapping_forward(&parent, &merge);
        assert!(mapping.is_empty());
    }

    #[test]
    fn test_find_line_author_basic() {
        let file_att = FileAttestation {
            file_path: "test.rs".to_string(),
            entries: vec![
                AttestationEntry {
                    hash: "ai_abc".to_string(),
                    line_ranges: vec![LineRange::Range(1, 3)],
                },
                AttestationEntry {
                    hash: "h_human".to_string(),
                    line_ranges: vec![LineRange::Range(4, 5)],
                },
            ],
        };

        let parent_content = "line1\nline2\nline3\nline4\nline5\n".to_string();
        let parent_data: Vec<(Option<String>, Option<&FileAttestation>)> =
            vec![(Some(parent_content), Some(&file_att))];

        // Merge has same lines in same order
        let merge_lines = vec!["line1", "line2", "line3", "line4", "line5"];

        // Line 2 in merge -> parent line 2 -> "ai_abc"
        let author = find_line_author(&file_att, 2, &parent_data, 0, &merge_lines);
        assert_eq!(author, Some("ai_abc".to_string()));

        // Line 4 in merge -> parent line 4 -> "h_human"
        let author = find_line_author(&file_att, 4, &parent_data, 0, &merge_lines);
        assert_eq!(author, Some("h_human".to_string()));
    }

    #[test]
    fn test_find_line_author_with_insertion() {
        let file_att = FileAttestation {
            file_path: "test.rs".to_string(),
            entries: vec![AttestationEntry {
                hash: "ai_abc".to_string(),
                line_ranges: vec![LineRange::Range(1, 2)],
            }],
        };

        let parent_content = "line1\nline2\n".to_string();
        let parent_data: Vec<(Option<String>, Option<&FileAttestation>)> =
            vec![(Some(parent_content), Some(&file_att))];

        // Merge has an extra line inserted
        let merge_lines = vec!["new_line", "line1", "line2"];

        // Line 2 in merge = "line1" -> parent line 1 -> "ai_abc"
        let author = find_line_author(&file_att, 2, &parent_data, 0, &merge_lines);
        assert_eq!(author, Some("ai_abc".to_string()));
    }

    #[test]
    fn test_is_merge_commit_detection() {
        // This test needs a real git repo, so we just verify the function
        // handles invalid paths gracefully
        let bad_path = Path::new("/nonexistent/path");
        assert!(!is_merge_commit(bad_path, "abc123"));
    }

    #[test]
    fn test_combine_metadata_merges_records() {
        use crate::core::authorship_log::{
            AUTHORSHIP_LOG_VERSION, AgentId, GIT_AI_VERSION, HumanRecord, PromptRecord,
        };
        use std::collections::BTreeMap;

        let mut prompts1 = BTreeMap::new();
        prompts1.insert(
            "abc123".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "claude".to_string(),
                    id: "s1".to_string(),
                    model: "opus".to_string(),
                },
                human_author: Some("dev@test.com".to_string()),
                messages_url: None,
                total_additions: 0,
                total_deletions: 0,
                accepted_lines: 0,
                overriden_lines: 0,
                custom_attributes: None,
            },
        );

        let mut humans2 = BTreeMap::new();
        humans2.insert(
            "h_def456".to_string(),
            HumanRecord {
                author: "dev2@test.com".to_string(),
            },
        );

        let log1 = AuthorshipLog {
            attestations: vec![],
            metadata: Metadata {
                schema_version: AUTHORSHIP_LOG_VERSION.to_string(),
                git_ai_version: Some(GIT_AI_VERSION.to_string()),
                base_commit_sha: "parent1".to_string(),
                prompts: prompts1,
                sessions: BTreeMap::new(),
                humans: BTreeMap::new(),
            },
        };

        let log2 = AuthorshipLog {
            attestations: vec![],
            metadata: Metadata {
                schema_version: AUTHORSHIP_LOG_VERSION.to_string(),
                git_ai_version: Some(GIT_AI_VERSION.to_string()),
                base_commit_sha: "parent2".to_string(),
                prompts: BTreeMap::new(),
                sessions: BTreeMap::new(),
                humans: humans2,
            },
        };

        let parent_notes = vec![("parent1".to_string(), log1), ("parent2".to_string(), log2)];

        let result = combine_metadata("merge_abc", &parent_notes);

        assert_eq!(result.base_commit_sha, "merge_abc");
        assert!(result.prompts.contains_key("abc123"));
        assert!(result.humans.contains_key("h_def456"));
    }
}
