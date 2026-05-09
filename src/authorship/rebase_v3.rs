//! Rebase attribution engine v3 - hunk-level transformation tracking.
//!
//! This module implements rebase attribution by tracking hunks through
//! transformations (copy/split/delete) rather than reconstructing from
//! commit-level mappings.
//!
//! Core principle: For any hunk h, attribution follows the transformation
//! applied to h. If h is copied, attribution is copied. If h is split,
//! attribution is split proportionally. If h is deleted, attribution is deleted.
//!
//! ## Entry Point
//!
//! The main entry point is `rewrite_authorship_after_rebase_v3()`, which should
//! be called from `rebase_authorship::rewrite_authorship_if_needed()` when a
//! feature flag is enabled.

use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::authorship_log::LineRange;
use crate::authorship::authorship_log_serialization::{
    AttestationEntry, AuthorshipLog, FileAttestation,
};
use crate::authorship::working_log::CheckpointKind;
use crate::error::GitAiError;
use crate::git::refs;
use crate::git::repository::Repository;
use std::collections::HashMap;

/// Maps original commits to new commits after rebase.
/// Handles many-to-one (squash), one-to-one (standard rebase), one-to-zero (drop).
#[derive(Debug, Clone)]
pub struct CommitMapping {
    /// Original commit SHA → New commit SHA(s)
    /// Empty vec = commit was dropped
    /// Multiple entries = commit was split (rare)
    pub original_to_new: HashMap<String, Vec<String>>,
    /// New commit SHA → Original commit SHA(s)
    /// Multiple entries = squash (many original commits → one new commit)
    pub new_to_original: HashMap<String, Vec<String>>,
}

impl CommitMapping {
    /// Build commit mapping from git's native range-diff.
    /// Uses git's sophisticated patch comparison that handles reword, conflicts, etc.
    pub fn from_rebase(
        repo: &Repository,
        original_commits: &[String],
        new_commits: &[String],
    ) -> Result<Self, GitAiError> {
        let mut original_to_new: HashMap<String, Vec<String>> = HashMap::new();
        let mut new_to_original: HashMap<String, Vec<String>> = HashMap::new();

        if original_commits.is_empty() || new_commits.is_empty() {
            return Ok(Self {
                original_to_new,
                new_to_original,
            });
        }

        // Get merge base for range-diff
        let merge_base = Self::get_merge_base(repo, &original_commits[0], &new_commits[0])?;

        // Build ranges for git range-diff
        let original_range = format!("{}..{}", merge_base, original_commits.last().unwrap());
        let new_range = format!("{}..{}", merge_base, new_commits.last().unwrap());

        // Run git range-diff to get native commit mapping
        let mut args = repo.global_args_for_exec();
        args.extend_from_slice(&["range-diff".to_string(), original_range, new_range]);

        let output = crate::git::repository::exec_git(&args)?;
        let stdout = String::from_utf8(output.stdout)?;

        // Parse range-diff output
        // Format:
        //   1:  a1b2c3d = 1:  b2c3d4e Standard rebase (identical)
        //   2:  c3d4e5f ! 2:  d4e5f6g Conflict resolution (fuzzy match)
        //   3:  e5f6g7h < -:  ------- Dropped commit
        //   -:  ------- > 3:  f6g7h8i New commit added
        for line in stdout.lines() {
            if line.contains(" = ") || line.contains(" ! ") {
                // Extract SHAs from "1:  a1b2c3d = 1:  b2c3d4e ..." format
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 5 {
                    let orig_sha = parts[1];
                    let new_sha = parts[4];

                    // Validate these are actual commit SHAs we're tracking
                    if Self::sha_in_list(orig_sha, original_commits)
                        && Self::sha_in_list(new_sha, new_commits)
                    {
                        original_to_new
                            .entry(orig_sha.to_string())
                            .or_default()
                            .push(new_sha.to_string());
                        new_to_original
                            .entry(new_sha.to_string())
                            .or_default()
                            .push(orig_sha.to_string());
                    }
                }
            }
        }

        // Fallback for unmapped commits: use subject-in-body heuristic for squashes
        // When Git squashes, it puts old subjects in the new commit body
        let unmapped_new: Vec<_> = new_commits
            .iter()
            .filter(|sha| !new_to_original.contains_key(*sha))
            .collect();
        let unmapped_orig: Vec<_> = original_commits
            .iter()
            .filter(|sha| !original_to_new.contains_key(*sha))
            .collect();

        if !unmapped_new.is_empty() && !unmapped_orig.is_empty() {
            for new_sha in unmapped_new {
                let new_body = Self::get_commit_body(repo, new_sha)?;

                for orig_sha in &unmapped_orig {
                    let orig_subject = Self::get_commit_subject(repo, orig_sha)?;

                    // Git squash puts original subjects in new body
                    if new_body.contains(&orig_subject) {
                        original_to_new
                            .entry(orig_sha.to_string())
                            .or_default()
                            .push(new_sha.to_string());
                        new_to_original
                            .entry(new_sha.to_string())
                            .or_default()
                            .push(orig_sha.to_string());
                    }
                }
            }
        }

        // Mark dropped commits (no mapping found)
        for orig_sha in original_commits {
            original_to_new.entry(orig_sha.clone()).or_default();
        }

        Ok(Self {
            original_to_new,
            new_to_original,
        })
    }

    fn get_merge_base(
        repo: &Repository,
        commit1: &str,
        commit2: &str,
    ) -> Result<String, GitAiError> {
        let mut args = repo.global_args_for_exec();
        args.extend_from_slice(&[
            "merge-base".to_string(),
            commit1.to_string(),
            commit2.to_string(),
        ]);

        let output = crate::git::repository::exec_git(&args)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    fn sha_in_list(sha: &str, list: &[String]) -> bool {
        list.iter()
            .any(|s| s.starts_with(sha) || sha.starts_with(&s[..7.min(s.len())]))
    }

    fn get_commit_subject(repo: &Repository, commit: &str) -> Result<String, GitAiError> {
        let mut args = repo.global_args_for_exec();
        args.extend_from_slice(&[
            "log".to_string(),
            "--format=%s".to_string(),
            "-n".to_string(),
            "1".to_string(),
            commit.to_string(),
        ]);

        let output = crate::git::repository::exec_git(&args)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    fn get_commit_body(repo: &Repository, commit: &str) -> Result<String, GitAiError> {
        let mut args = repo.global_args_for_exec();
        args.extend_from_slice(&[
            "log".to_string(),
            "--format=%b".to_string(),
            "-n".to_string(),
            "1".to_string(),
            commit.to_string(),
        ]);

        let output = crate::git::repository::exec_git(&args)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }
}

/// Diff hunk from git diff output (compatible with v2's format)
#[derive(Debug, Clone)]
struct DiffHunk {
    old_start: u32,
    old_count: u32,
    // Note: new_start is parsed but not stored - the algorithm only needs
    // old_start/old_count to find preserved segments and new_count for offset calculation
    new_count: u32,
}

/// Mapping of file paths to their diff hunks
type HunksMap = HashMap<String, Vec<DiffHunk>>;

/// Mapping of old file paths to new file paths (for renames)
type RenameMap = HashMap<String, String>;

/// Result type for diff parsing operations
type DiffParseResult = Result<(HunksMap, RenameMap), GitAiError>;

/// Apply diff hunks to line attributions, shifting line numbers appropriately.
/// Borrowed from v2's proven implementation.
///
/// Algorithm:
/// 1. Build "preserved segments" - ranges of old lines that survive, with offset
/// 2. For each attribution, intersect with preserved segments
/// 3. Apply offset to get new line numbers
fn apply_hunks_to_line_attributions(
    old_attrs: &[LineAttribution],
    hunks: &[DiffHunk],
) -> Vec<LineAttribution> {
    if hunks.is_empty() {
        return old_attrs.to_vec();
    }

    // Build preserved segments: ranges of old line numbers that survive and their offset
    let mut segments: Vec<(u32, u32, i64)> = Vec::with_capacity(hunks.len() + 1);
    let mut offset: i64 = 0;
    let mut prev_old_end: u32 = 1; // 1-indexed

    for hunk in hunks {
        // Preserved segment before this hunk
        if prev_old_end < hunk.old_start + 1 {
            let seg_end = if hunk.old_count == 0 {
                hunk.old_start // Pure insertion: preserve up to and including old_start
            } else {
                hunk.old_start.saturating_sub(1) // up to but not including the hunk
            };
            if prev_old_end <= seg_end {
                segments.push((prev_old_end, seg_end, offset));
            }
        }

        // Update offset based on this hunk
        offset += hunk.new_count as i64 - hunk.old_count as i64;

        prev_old_end = if hunk.old_count == 0 {
            hunk.old_start + 1 // after insertion point
        } else {
            hunk.old_start + hunk.old_count // after deleted range
        };
    }

    // Final segment after last hunk
    segments.push((prev_old_end, u32::MAX, offset));

    // Apply mapping to each attribution
    let mut new_attrs: Vec<LineAttribution> = Vec::with_capacity(old_attrs.len());

    for attr in old_attrs {
        for &(seg_start, seg_end, seg_offset) in &segments {
            let range_start = attr.start_line.max(seg_start);
            let range_end = attr.end_line.min(seg_end);

            if range_start <= range_end {
                let new_start = (range_start as i64 + seg_offset).max(1) as u32;
                let new_end = (range_end as i64 + seg_offset).max(1) as u32;
                new_attrs.push(LineAttribution {
                    start_line: new_start,
                    end_line: new_end,
                    author_id: attr.author_id.clone(),
                    overrode: attr.overrode.clone(),
                });
            }
        }
    }

    new_attrs
}

/// Parse a unified diff hunk header line like `@@ -10,5 +12,6 @@`
fn parse_hunk_header(line: &str) -> Option<DiffHunk> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 || parts[0] != "@@" {
        return None;
    }

    let old_part = parts[1].trim_start_matches('-');
    let new_part = parts[2].trim_start_matches('+');

    let (old_start, old_count) = parse_range_spec(old_part)?;
    let (_new_start, new_count) = parse_range_spec(new_part)?;

    Some(DiffHunk {
        old_start,
        old_count,
        new_count,
    })
}

/// Parse a range spec like "10,5" or "10" (count defaults to 1)
fn parse_range_spec(spec: &str) -> Option<(u32, u32)> {
    if let Some((start_str, count_str)) = spec.split_once(',') {
        let start = start_str.parse().ok()?;
        let count = count_str.parse().ok()?;
        Some((start, count))
    } else {
        let start = spec.parse().ok()?;
        Some((start, 1))
    }
}

/// Fast path: check if tracked files have identical tree blobs between original and new commit.
/// If true, we can skip diff-tree entirely and just copy the note with updated base_commit_sha.
///
/// This is the "nothing happens" optimization - when rebasing onto a branch that only modified
/// unrelated files, the AI-tracked files have identical blobs and we can skip all diff logic.
fn tracked_files_unchanged(
    repo: &Repository,
    original_commit: &str,
    new_commit: &str,
    tracked_files: &[String],
) -> Result<bool, GitAiError> {
    if tracked_files.is_empty() {
        return Ok(false);
    }

    // Run git diff-tree --raw to check if any tracked files changed
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "diff-tree".to_string(),
        "-r".to_string(),
        "--raw".to_string(),
        "-z".to_string(),
        "--no-abbrev".to_string(),
        original_commit.to_string(),
        new_commit.to_string(),
        "--".to_string(),
    ]);
    args.extend(tracked_files.iter().cloned());

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = output.stdout;

    // If output contains any ':' delta lines, tracked files changed
    for &byte in &stdout {
        if byte == b':' {
            return Ok(false);
        }
    }

    // No deltas found - tracked files are identical
    Ok(true)
}

/// Get diff hunks between two commits, grouped by file path.
/// Also returns rename mappings (old path → new path).
fn get_diff_hunks(repo: &Repository, original_commit: &str, new_commit: &str) -> DiffParseResult {
    // Run git diff-tree to get unified diff with hunks
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "diff-tree".to_string(),
        "-r".to_string(),
        "-p".to_string(),
        "-M".to_string(),  // Detect renames
        "-U0".to_string(), // No context lines, just hunks
        "--no-color".to_string(),
        original_commit.to_string(),
        new_commit.to_string(),
    ]);

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)?;

    parse_diff_tree_output(&stdout)
}

/// Parse git diff-tree output to extract hunks grouped by file.
/// Also extracts rename mappings (old path → new path).
fn parse_diff_tree_output(diff_output: &str) -> DiffParseResult {
    let mut hunks_by_file: HunksMap = HashMap::new();
    let mut renames: HashMap<String, String> = HashMap::new();
    let mut current_file: Option<String> = None;
    let mut rename_from: Option<String> = None;

    for line in diff_output.lines() {
        if let Some(old_path) = line.strip_prefix("rename from ") {
            // Track rename source
            rename_from = Some(old_path.to_string());
        } else if let Some(new_path) = line.strip_prefix("rename to ") {
            // Track rename destination
            if let Some(old_path) = rename_from.take() {
                renames.insert(old_path, new_path.to_string());
                current_file = Some(new_path.to_string());
            }
        } else if let Some(file_path) = line.strip_prefix("+++ b/") {
            // New file being diffed (not a rename)
            current_file = Some(file_path.to_string());
        } else if line.starts_with("@@") {
            // Hunk header
            if let Some(ref file) = current_file {
                if let Some(hunk) = parse_hunk_header(line) {
                    hunks_by_file.entry(file.clone()).or_default().push(hunk);
                }
            }
        }
    }

    Ok((hunks_by_file, renames))
}

/// Convert line ranges to line attributions for hunk transformation.
fn line_ranges_to_line_attributions(
    line_ranges: &[LineRange],
    author_id: &str,
) -> Vec<LineAttribution> {
    let mut attrs = Vec::new();

    for line_range in line_ranges {
        let (start, end) = match line_range {
            LineRange::Single(line) => (*line, *line),
            LineRange::Range(start, end) => (*start, *end),
        };

        attrs.push(LineAttribution {
            start_line: start,
            end_line: end,
            author_id: author_id.to_string(),
            overrode: None,
        });
    }

    attrs
}

/// Convert line attributions back to line ranges.
fn line_attributions_to_line_ranges(attrs: &[LineAttribution]) -> Vec<LineRange> {
    let mut line_ranges = Vec::new();

    for attr in attrs {
        let line_range = if attr.start_line == attr.end_line {
            LineRange::Single(attr.start_line)
        } else {
            LineRange::Range(attr.start_line, attr.end_line)
        };
        line_ranges.push(line_range);
    }

    line_ranges
}

/// Get the parent SHA of a commit.
fn get_parent_sha(repo: &Repository, commit: &str) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&["rev-parse".to_string(), format!("{}^", commit)]);

    let output = crate::git::repository::exec_git(&args)?;
    let parent = String::from_utf8(output.stdout)?.trim().to_string();

    Ok(parent)
}

/// Build authorship note from working log checkpoint data (for conflict resolution).
/// This is v3's equivalent of v2's `build_note_from_conflict_wl()`.
fn build_note_from_working_log(
    repo: &Repository,
    new_commit: &str,
    parent_sha: &str,
) -> Result<Option<String>, GitAiError> {
    use crate::authorship::authorship_log_serialization::generate_short_hash;
    use crate::authorship::rebase_authorship::build_file_attestation_from_line_attributions;

    // Try to read working log for the parent commit
    let working_log = match repo.storage.working_log_for_base_commit(parent_sha) {
        Ok(wl) => wl,
        Err(_) => {
            tracing::debug!(
                "rebase_v3: No working log found for parent {}, cannot build note from checkpoints",
                &parent_sha[..8]
            );
            return Ok(None);
        }
    };

    let checkpoints = match working_log.read_all_checkpoints() {
        Ok(cp) => cp,
        Err(_) => {
            tracing::debug!(
                "rebase_v3: Failed to read checkpoints from working log for {}",
                &parent_sha[..8]
            );
            return Ok(None);
        }
    };

    let mut authorship_log = AuthorshipLog::new();
    authorship_log.metadata.base_commit_sha = new_commit.to_string();

    // Collect line attributions per file from AI checkpoints
    let mut file_line_attrs: HashMap<String, Vec<LineAttribution>> = HashMap::new();
    let mut has_ai_content = false;

    for checkpoint in &checkpoints {
        // Skip legacy human checkpoints
        if checkpoint.kind == CheckpointKind::Human {
            continue;
        }

        // Record known humans in metadata
        if checkpoint.kind == CheckpointKind::KnownHuman {
            let hash = crate::authorship::authorship_log_serialization::generate_human_short_hash(
                &checkpoint.author,
            );
            authorship_log
                .metadata
                .humans
                .entry(hash)
                .or_insert_with(|| crate::authorship::authorship_log::HumanRecord {
                    author: checkpoint.author.clone(),
                });
            continue;
        }

        // AI checkpoints need an agent_id
        let agent_id = match &checkpoint.agent_id {
            Some(id) => id,
            None => continue,
        };

        // Record in metadata (sessions for new format, prompts for old format)
        if checkpoint.trace_id.is_some() {
            let session_id = crate::authorship::authorship_log_serialization::generate_session_id(
                &agent_id.id,
                &agent_id.tool,
            );
            authorship_log
                .metadata
                .sessions
                .entry(session_id)
                .or_insert_with(|| crate::authorship::authorship_log::SessionRecord {
                    agent_id: agent_id.clone(),
                    human_author: None,
                    custom_attributes: None,
                });
        } else {
            let author_id = generate_short_hash(&agent_id.id, &agent_id.tool);
            authorship_log
                .metadata
                .prompts
                .entry(author_id)
                .or_insert_with(|| crate::authorship::authorship_log::PromptRecord {
                    agent_id: agent_id.clone(),
                    human_author: None,
                    total_additions: checkpoint.line_stats.additions,
                    total_deletions: checkpoint.line_stats.deletions,
                    accepted_lines: 0,
                    overriden_lines: 0,
                    custom_attributes: None,
                    messages_url: None,
                });
        }

        // Collect line attributions for each file
        for entry in &checkpoint.entries {
            if entry.line_attributions.is_empty() {
                continue;
            }
            file_line_attrs
                .entry(entry.file.clone())
                .or_default()
                .extend(entry.line_attributions.iter().cloned());
        }
    }

    // Build FileAttestations from line attributions
    let mut accepted_per_author: HashMap<String, u32> = HashMap::new();
    for (file_path, line_attrs) in &file_line_attrs {
        // Tally accepted lines per author
        for la in line_attrs {
            *accepted_per_author.entry(la.author_id.clone()).or_insert(0) +=
                la.end_line - la.start_line + 1;
        }
        if let Some(file_att) = build_file_attestation_from_line_attributions(file_path, line_attrs)
        {
            authorship_log.attestations.push(file_att);
            has_ai_content = true;
        }
    }

    // Update accepted_lines counts in prompt metadata
    for (author_id, count) in accepted_per_author {
        if let Some(record) = authorship_log.metadata.prompts.get_mut(&author_id) {
            record.accepted_lines = count;
        }
    }

    if !has_ai_content {
        tracing::debug!(
            "rebase_v3: Working log for {} has no AI content",
            &parent_sha[..8]
        );
        return Ok(None);
    }

    authorship_log
        .serialize_to_string()
        .map(Some)
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))
}

/// Main entry point for rebase attribution (v3 implementation).
///
/// Rewrites authorship notes for rebased commits by tracking hunk transformations.
/// This is intended to replace `rewrite_authorship_after_rebase_v2` once validated.
///
/// # Arguments
/// * `repo` - Git repository
/// * `original_head` - SHA of original HEAD before rebase
/// * `original_commits` - List of original commit SHAs (oldest-first)
/// * `new_commits` - List of new commit SHAs after rebase (oldest-first)
/// * `_human_author` - Human author name (for metadata)
///
/// # Returns
/// Ok(()) if attribution was successfully rewritten, Err otherwise.
pub fn rewrite_authorship_after_rebase_v3(
    repo: &Repository,
    _original_head: &str,
    original_commits: &[String],
    new_commits: &[String],
    _human_author: &str,
) -> Result<(), GitAiError> {
    tracing::debug!(
        "rebase_v3: Rewriting attribution for {} original commits -> {} new commits",
        original_commits.len(),
        new_commits.len()
    );

    // Early exit: no commits to process
    if new_commits.is_empty() {
        if !original_commits.is_empty() {
            // Park orphaned notes for recovery (same as v2)
            // This happens when rebase produces no new commits (e.g., already-applied patches)
            crate::authorship::rebase_authorship::park_orphaned_notes_for_recovery(
                repo,
                _original_head,
                original_commits,
            )
            .ok(); // Non-fatal: don't fail the rebase if parking fails
        }
        tracing::debug!("rebase_v3: No new commits, nothing to do");
        return Ok(());
    }

    // Step 1: Build commit mapping (original -> new)
    let commit_mapping = CommitMapping::from_rebase(repo, original_commits, new_commits)?;

    tracing::debug!(
        "rebase_v3: Built commit mapping: {} original -> {} new",
        commit_mapping.original_to_new.len(),
        commit_mapping.new_to_original.len()
    );

    // Step 2: For each new commit, copy notes from corresponding original commit(s)
    for new_commit in new_commits {
        tracing::debug!("rebase_v3: Processing new commit {}", &new_commit[..8]);

        let original_commits_for_new = match commit_mapping.new_to_original.get(new_commit) {
            Some(originals) => originals,
            None => {
                tracing::debug!(
                    "rebase_v3: No original commit mapped to new commit {}, skipping",
                    &new_commit[..8]
                );
                continue;
            }
        };

        tracing::debug!(
            "rebase_v3: Mapped {} original commits to new commit {}",
            original_commits_for_new.len(),
            &new_commit[..8]
        );

        // Simple case: one original commit maps to one new commit
        if original_commits_for_new.len() == 1 {
            let original_commit = &original_commits_for_new[0];

            // Check if new commit already has a note with actual attestations
            // Empty notes (no attestations) can arise when post-commit hook fires during
            // `rebase --continue` for a human-resolved conflict commit - in that case
            // we must still process to transfer attribution for AI lines that survived.
            if let Some(note_content) = refs::show_authorship_note(repo, new_commit) {
                if let Ok(existing_log) = AuthorshipLog::deserialize_from_string(&note_content) {
                    if !existing_log.attestations.is_empty() {
                        tracing::info!(
                            "rebase_v3: New commit {} already has note with {} attestations (from trace2), skipping",
                            &new_commit[..8],
                            existing_log.attestations.len()
                        );
                        continue;
                    } else {
                        tracing::debug!(
                            "rebase_v3: New commit {} has empty note (conflict resolution), will transform from original",
                            &new_commit[..8]
                        );
                    }
                } else {
                    tracing::debug!(
                        "rebase_v3: New commit {} has unparseable note, will transform from original",
                        &new_commit[..8]
                    );
                }
            } else {
                tracing::debug!(
                    "rebase_v3: New commit {} has no note yet, will transform from original",
                    &new_commit[..8]
                );
            }

            // Try to build note from working log first (conflict resolution case)
            let parent_sha = get_parent_sha(repo, new_commit)?;
            let note_from_wl = build_note_from_working_log(repo, new_commit, &parent_sha)?;

            if let Some(note) = note_from_wl {
                // Found working log data - use it directly
                refs::notes_add(repo, new_commit, &note)?;
                tracing::debug!(
                    "rebase_v3: Built note for {} from working log (conflict resolution)",
                    &new_commit[..8]
                );
                continue;
            }

            // No working log - transform from original commit's note
            if let Some(note_content) = refs::show_authorship_note(repo, original_commit) {
                if let Ok(mut log) = AuthorshipLog::deserialize_from_string(&note_content) {
                    // Fast path: check if tracked files are unchanged
                    let tracked_files: Vec<String> = log
                        .attestations
                        .iter()
                        .map(|a| a.file_path.clone())
                        .collect();

                    if tracked_files_unchanged(repo, original_commit, new_commit, &tracked_files)? {
                        // Fast path: tracked files unchanged, just copy note with updated SHA
                        log.metadata.base_commit_sha = new_commit.clone();
                        let updated_note = log.serialize_to_string().map_err(|_| {
                            GitAiError::Generic("Failed to serialize authorship log".to_string())
                        })?;
                        refs::notes_add(repo, new_commit, &updated_note)?;

                        tracing::debug!(
                            "rebase_v3: Fast path - copied note {} -> {} (tracked files unchanged)",
                            &original_commit[..8],
                            &new_commit[..8]
                        );
                        continue;
                    }

                    // Slow path: tracked files changed, need to transform via hunks
                    // Get diff hunks and rename mappings between original and new commits
                    let (hunks, renames) = get_diff_hunks(repo, original_commit, new_commit)?;

                    // Transform each file's attestations using hunks
                    let mut transformed_attestations = Vec::new();

                    for file_attestation in &log.attestations {
                        // Check if file was renamed
                        let new_file_path = renames
                            .get(&file_attestation.file_path)
                            .unwrap_or(&file_attestation.file_path);

                        // Get hunks for this file (use new path if renamed)
                        let empty_hunks = Vec::new();
                        let file_hunks = hunks.get(new_file_path).unwrap_or(&empty_hunks);

                        // Transform each entry's line ranges
                        let mut transformed_entries = Vec::new();

                        for entry in &file_attestation.entries {
                            // Convert to line attributions
                            let old_attrs =
                                line_ranges_to_line_attributions(&entry.line_ranges, &entry.hash);

                            // Apply hunks
                            let new_attrs =
                                apply_hunks_to_line_attributions(&old_attrs, file_hunks);

                            // Convert back to line ranges
                            let new_line_ranges = line_attributions_to_line_ranges(&new_attrs);

                            if !new_line_ranges.is_empty() {
                                transformed_entries.push(AttestationEntry {
                                    hash: entry.hash.clone(),
                                    line_ranges: new_line_ranges,
                                });
                            }
                        }

                        if !transformed_entries.is_empty() {
                            transformed_attestations.push(FileAttestation {
                                file_path: new_file_path.clone(), // Use new path if renamed
                                entries: transformed_entries,
                            });
                        }
                    }

                    // Update log with transformed attestations
                    log.attestations = transformed_attestations;
                    log.metadata.base_commit_sha = new_commit.clone();

                    // Write to new commit
                    let updated_note = log.serialize_to_string().map_err(|_| {
                        GitAiError::Generic("Failed to serialize authorship log".to_string())
                    })?;
                    refs::notes_add(repo, new_commit, &updated_note)?;

                    tracing::debug!(
                        "rebase_v3: Transformed note {} -> {} ({} attestations)",
                        &original_commit[..8],
                        &new_commit[..8],
                        log.attestations.len()
                    );
                }
            } else {
                tracing::debug!(
                    "rebase_v3: No note found for original commit {}, skipping",
                    &original_commit[..8]
                );
            }
        } else {
            // Squash case: multiple original commits -> one new commit
            tracing::debug!(
                "rebase_v3: Squash detected ({} original commits -> 1 new), merging notes",
                original_commits_for_new.len()
            );

            // Try working log first (conflict resolution during squash)
            let parent_sha = get_parent_sha(repo, new_commit)?;
            let note_from_wl = build_note_from_working_log(repo, new_commit, &parent_sha)?;

            if let Some(note) = note_from_wl {
                refs::notes_add(repo, new_commit, &note)?;
                tracing::debug!(
                    "rebase_v3: Built squashed note for {} from working log",
                    &new_commit[..8]
                );
                continue;
            }

            // Merge all original commits' notes
            let mut merged_log = AuthorshipLog::new();
            merged_log.metadata.base_commit_sha = new_commit.clone();

            // Collect attestations and metadata from all original commits
            let mut file_line_attrs: HashMap<String, Vec<LineAttribution>> = HashMap::new();

            for original_commit in original_commits_for_new {
                if let Some(note_content) = refs::show_authorship_note(repo, original_commit) {
                    if let Ok(log) = AuthorshipLog::deserialize_from_string(&note_content) {
                        // Merge metadata (prompts, humans, sessions)
                        for (hash, prompt) in log.metadata.prompts {
                            merged_log.metadata.prompts.entry(hash).or_insert(prompt);
                        }
                        for (hash, human) in log.metadata.humans {
                            merged_log.metadata.humans.entry(hash).or_insert(human);
                        }
                        for (session_id, session) in log.metadata.sessions {
                            merged_log
                                .metadata
                                .sessions
                                .entry(session_id)
                                .or_insert(session);
                        }

                        // Convert attestations to line attributions and collect by file
                        for file_attestation in &log.attestations {
                            for entry in &file_attestation.entries {
                                let attrs = line_ranges_to_line_attributions(
                                    &entry.line_ranges,
                                    &entry.hash,
                                );
                                file_line_attrs
                                    .entry(file_attestation.file_path.clone())
                                    .or_default()
                                    .extend(attrs);
                            }
                        }
                    }
                }
            }

            // Build FileAttestations from merged line attributions
            for (file_path, line_attrs) in file_line_attrs {
                if let Some(file_att) = crate::authorship::rebase_authorship::build_file_attestation_from_line_attributions(&file_path, &line_attrs) {
                    merged_log.attestations.push(file_att);
                }
            }

            // Write merged note
            if !merged_log.attestations.is_empty() {
                let merged_note = merged_log.serialize_to_string().map_err(|_| {
                    GitAiError::Generic("Failed to serialize merged authorship log".to_string())
                })?;
                refs::notes_add(repo, new_commit, &merged_note)?;

                tracing::debug!(
                    "rebase_v3: Merged {} original commits into new commit {} ({} attestations)",
                    original_commits_for_new.len(),
                    &new_commit[..8],
                    merged_log.attestations.len()
                );
            } else {
                tracing::debug!(
                    "rebase_v3: Squash produced no attestations for {}",
                    &new_commit[..8]
                );
            }
        }
    }

    tracing::debug!("rebase_v3: Attribution rewrite complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    // Note: V3 is comprehensively tested via integration tests in tests/integration/.
    // Unit tests for internal functions can be added here as needed, but integration
    // tests provide better coverage of the complete rebase attribution flow.
}
