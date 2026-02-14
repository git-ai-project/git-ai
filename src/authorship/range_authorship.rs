use std::collections::HashMap;
use std::collections::HashSet;

use serde::Deserialize;
use serde::Serialize;

use crate::authorship::diff_ai_accepted::diff_ai_accepted_stats;
use crate::authorship::ignore::{build_ignore_matcher, should_ignore_file_with_matcher};
use crate::authorship::stats::{CommitStats, stats_for_commit_stats, stats_from_authorship_log};
use crate::error::GitAiError;
use crate::git::refs::{CommitAuthorship, get_commits_with_notes_from_list};
use crate::git::repository::{CommitRange, Repository};
use crate::utils::debug_log;

use std::io::IsTerminal;

/// The git empty tree hash - represents an empty repository state
/// This is the hash of the empty tree object that git uses internally
const EMPTY_TREE_HASH: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Check if a file path should be ignored based on the provided patterns
/// Supports both exact matches and glob patterns (e.g., "*.lock", "**/*.generated.js")
#[allow(dead_code)] // Kept for downstream compatibility.
pub fn should_ignore_file(path: &str, ignore_patterns: &[String]) -> bool {
    crate::authorship::ignore::should_ignore_file(path, ignore_patterns)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeAuthorshipStats {
    pub authorship_stats: RangeAuthorshipStatsData,
    pub range_stats: CommitStats,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeAuthorshipStatsData {
    pub total_commits: usize,
    pub commits_with_authorship: usize,
    pub authors_committing_authorship: HashSet<String>,
    pub authors_not_committing_authorship: HashSet<String>,
    pub commits_without_authorship: Vec<String>,
    pub commits_without_authorship_with_authors: Vec<(String, String)>, // (sha, git_author)
}

pub fn range_authorship(
    commit_range: CommitRange,
    pre_fetch_contents: bool,
    ignore_patterns: &[String],
) -> Result<RangeAuthorshipStats, GitAiError> {
    commit_range.is_valid()?;

    // Fetch the branch if pre_fetch_contents is true
    if pre_fetch_contents {
        let repository = commit_range.repo();
        let refname = &commit_range.refname;

        // Get default remote, fallback to "origin" if not found
        let default_remote = repository
            .get_default_remote()?
            .unwrap_or_else(|| "origin".to_string());

        // Extract remote and branch from refname
        let (remote, fetch_refspec) = if refname.starts_with("refs/remotes/") {
            // Remote branch: refs/remotes/origin/branch-name -> origin, refs/heads/branch-name
            let without_prefix = refname.strip_prefix("refs/remotes/").unwrap();
            let parts: Vec<&str> = without_prefix.splitn(2, '/').collect();
            if parts.len() == 2 {
                (parts[0].to_string(), format!("refs/heads/{}", parts[1]))
            } else {
                (default_remote.clone(), refname.to_string())
            }
        } else if refname.starts_with("refs/heads/") {
            // Local branch: refs/heads/branch-name -> default_remote, refs/heads/branch-name
            (default_remote.clone(), refname.to_string())
        } else if refname.contains('/') && !refname.starts_with("refs/") {
            // Simple remote format: origin/branch-name -> origin, refs/heads/branch-name
            let parts: Vec<&str> = refname.splitn(2, '/').collect();
            if parts.len() == 2 {
                (parts[0].to_string(), format!("refs/heads/{}", parts[1]))
            } else {
                (default_remote.clone(), format!("refs/heads/{}", refname))
            }
        } else {
            // Plain branch name: branch-name -> default_remote, refs/heads/branch-name
            (default_remote.clone(), format!("refs/heads/{}", refname))
        };

        let mut args = repository.global_args_for_exec();
        args.push("fetch".to_string());
        args.push(remote.clone());
        args.push(fetch_refspec.clone());

        let output = crate::git::repository::exec_git(&args)?;

        if !output.status.success() {
            return Err(GitAiError::Generic(format!(
                "Failed to fetch {} from {}: {}",
                fetch_refspec,
                remote,
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        debug_log(&format!("✓ Fetched {} from {}", fetch_refspec, remote));
    }

    // Clone commit_range before consuming it
    let repository = commit_range.repo();
    let commit_range_clone = commit_range.clone();

    // Collect commit SHAs from the range
    let commit_shas: Vec<String> = commit_range
        .into_iter()
        .map(|c| c.id().to_string())
        .collect();
    let commit_authorship = get_commits_with_notes_from_list(repository, &commit_shas)?;

    // Calculate range stats - now just pass start, end, and commits
    let range_stats =
        calculate_range_stats_direct(repository, commit_range_clone, ignore_patterns)?;

    Ok(RangeAuthorshipStats {
        authorship_stats: RangeAuthorshipStatsData {
            total_commits: commit_authorship.len(),
            commits_with_authorship: commit_authorship
                .iter()
                .filter(|ca| matches!(ca, CommitAuthorship::Log { .. }))
                .count(),
            authors_committing_authorship: commit_authorship
                .iter()
                .filter_map(|ca| match ca {
                    CommitAuthorship::Log { git_author, .. } => Some(git_author.clone()),
                    _ => None,
                })
                .collect(),
            authors_not_committing_authorship: commit_authorship
                .iter()
                .filter_map(|ca| match ca {
                    CommitAuthorship::NoLog { git_author, .. } => Some(git_author.clone()),
                    _ => None,
                })
                .collect(),
            commits_without_authorship: commit_authorship
                .iter()
                .filter_map(|ca| match ca {
                    CommitAuthorship::NoLog { sha, .. } => Some(sha.clone()),
                    _ => None,
                })
                .collect(),
            commits_without_authorship_with_authors: commit_authorship
                .iter()
                .filter_map(|ca| match ca {
                    CommitAuthorship::NoLog { sha, git_author } => {
                        Some((sha.clone(), git_author.clone()))
                    }
                    _ => None,
                })
                .collect(),
        },
        range_stats,
    })
}

/// Create an in-memory authorship log for a commit range by treating it as a squash
/// Similar to rewrite_authorship_after_squash_or_rebase but tailored for ranges
fn create_authorship_log_for_range(
    repo: &Repository,
    start_sha: &str,
    end_sha: &str,
    commit_shas: &[String],
    ignore_patterns: &[String],
) -> Result<crate::authorship::authorship_log_serialization::AuthorshipLog, GitAiError> {
    use crate::authorship::virtual_attribution::{
        VirtualAttributions, merge_attributions_favoring_first,
    };

    debug_log(&format!(
        "Calculating authorship log for range: {} -> {}",
        start_sha, end_sha
    ));

    // Step 1: Get list of changed files between the two commits
    let all_changed_files = repo.diff_changed_files(start_sha, end_sha)?;
    let ignore_matcher = build_ignore_matcher(ignore_patterns);

    // Filter out ignored files from the changed files
    let changed_files: Vec<String> = all_changed_files
        .into_iter()
        .filter(|file| !should_ignore_file_with_matcher(file, &ignore_matcher))
        .collect();

    // Note: We intentionally do NOT filter to AI-touched files here.
    // For range authorship, AI lines may have been introduced in commits BEFORE the range
    // and still exist in the end state. We need to process all changed files and let
    // VirtualAttributions find the correct authorship from git blame history.

    if changed_files.is_empty() {
        // No files changed, return empty authorship log
        debug_log("No files changed in range");
        return Ok(
            crate::authorship::authorship_log_serialization::AuthorshipLog {
                attestations: Vec::new(),
                metadata: crate::authorship::authorship_log_serialization::AuthorshipMetadata {
                    schema_version: "3".to_string(),
                    git_ai_version: Some(
                        crate::authorship::authorship_log_serialization::GIT_AI_VERSION.to_string(),
                    ),
                    base_commit_sha: end_sha.to_string(),
                    prompts: std::collections::BTreeMap::new(),
                },
            },
        );
    }

    debug_log(&format!(
        "Processing {} changed files for range authorship",
        changed_files.len()
    ));

    // Special handling for empty tree: there's no start state to compare against
    // We only need the end state's attributions
    if start_sha == EMPTY_TREE_HASH {
        debug_log("Start is empty tree - using only end commit attributions");

        let repo_clone = repo.clone();
        let mut end_va = smol::block_on(async {
            VirtualAttributions::new_for_base_commit(
                repo_clone,
                end_sha.to_string(),
                &changed_files,
                None,
            )
            .await
        })?;

        // Filter to only include prompts from commits in this range
        let commit_set: HashSet<String> = commit_shas.iter().cloned().collect();
        end_va.filter_to_commits(&commit_set);

        // Convert to AuthorshipLog
        let mut authorship_log = end_va.to_authorship_log()?;
        authorship_log.metadata.base_commit_sha = end_sha.to_string();

        debug_log(&format!(
            "Created authorship log with {} attestations, {} prompts",
            authorship_log.attestations.len(),
            authorship_log.metadata.prompts.len()
        ));

        return Ok(authorship_log);
    }

    // Step 2: Create VirtualAttributions for start commit (older)
    // Pass start_sha as blame_start_commit to limit blame scope to the range,
    // avoiding expensive traversal of the entire repository history
    let repo_clone = repo.clone();
    let start_sha_limit = Some(start_sha.to_string());
    let mut start_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            start_sha.to_string(),
            &changed_files,
            start_sha_limit,
        )
        .await
    })?;

    // Step 3: Create VirtualAttributions for end commit (newer)
    // Pass start_sha as blame_start_commit to limit blame scope to the range
    let repo_clone = repo.clone();
    let start_sha_limit = Some(start_sha.to_string());
    let mut end_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            end_sha.to_string(),
            &changed_files,
            start_sha_limit,
        )
        .await
    })?;

    // Step 3.5: Filter both VirtualAttributions to only include prompts from commits in this range
    // This ensures we only count AI contributions that happened during these commits,
    // not AI contributions from before the range
    let commit_set: HashSet<String> = commit_shas.iter().cloned().collect();
    start_va.filter_to_commits(&commit_set);
    end_va.filter_to_commits(&commit_set);

    // Step 4: Read committed files from end commit (final state)
    let committed_files = get_committed_files_content(repo, end_sha, &changed_files)?;

    debug_log(&format!(
        "Read {} committed files from end commit",
        committed_files.len()
    ));

    // Step 5: Merge VirtualAttributions, favoring end commit (newer state)
    let merged_va = merge_attributions_favoring_first(end_va, start_va, committed_files)?;

    // Step 6: Convert to AuthorshipLog
    let mut authorship_log = merged_va.to_authorship_log()?;
    authorship_log.metadata.base_commit_sha = end_sha.to_string();

    debug_log(&format!(
        "Created authorship log with {} attestations, {} prompts",
        authorship_log.attestations.len(),
        authorship_log.metadata.prompts.len()
    ));

    Ok(authorship_log)
}

/// Get file contents from a commit tree for specified pathspecs
fn get_committed_files_content(
    repo: &Repository,
    commit_sha: &str,
    pathspecs: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    let commit = repo.find_commit(commit_sha.to_string())?;
    let tree = commit.tree()?;

    let mut files = HashMap::new();

    for file_path in pathspecs {
        match tree.get_path(std::path::Path::new(file_path)) {
            Ok(entry) => {
                if let Ok(blob) = repo.find_blob(entry.id()) {
                    let blob_content = blob.content().unwrap_or_default();
                    let content = String::from_utf8_lossy(&blob_content).to_string();
                    files.insert(file_path.clone(), content);
                }
            }
            Err(_) => {
                // File doesn't exist in this commit (could be deleted), skip it
            }
        }
    }

    Ok(files)
}

/// Get git diff statistics for a commit range (start..end)
fn get_git_diff_stats_for_range(
    repo: &Repository,
    start_sha: &str,
    end_sha: &str,
    ignore_patterns: &[String],
) -> Result<(u32, u32), GitAiError> {
    // Use git diff --numstat to get diff statistics for the range
    let mut args = repo.global_args_for_exec();
    args.push("diff".to_string());
    args.push("--numstat".to_string());
    args.push(format!("{}..{}", start_sha, end_sha));

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut added_lines = 0u32;
    let mut deleted_lines = 0u32;
    let ignore_matcher = build_ignore_matcher(ignore_patterns);

    // Parse numstat output
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // Parse numstat format: "added\tdeleted\tfilename"
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            // Check if this file should be ignored and skip it
            let filename = parts[2];
            if should_ignore_file_with_matcher(filename, &ignore_matcher) {
                continue;
            }

            // Parse added lines
            if let Ok(added) = parts[0].parse::<u32>() {
                added_lines += added;
            }

            // Parse deleted lines (handle "-" for binary files)
            if parts[1] != "-"
                && let Ok(deleted) = parts[1].parse::<u32>()
            {
                deleted_lines += deleted;
            }
        }
    }

    Ok((added_lines, deleted_lines))
}

/// Calculate AI vs human line contributions for a commit range
/// Uses VirtualAttributions approach to create an in-memory squash
fn calculate_range_stats_direct(
    repo: &Repository,
    commit_range: CommitRange,
    ignore_patterns: &[String],
) -> Result<CommitStats, GitAiError> {
    let start_sha = commit_range.start_oid.clone();
    let end_sha = commit_range.end_oid.clone();
    // Special case: single commit range (start == end)
    if start_sha == end_sha {
        return stats_for_commit_stats(repo, &end_sha, ignore_patterns);
    }

    // Step 1: Get git diff stats between start and end
    let (git_diff_added_lines, git_diff_deleted_lines) =
        get_git_diff_stats_for_range(repo, &start_sha, &end_sha, ignore_patterns)?;

    let diff_ai_stats = diff_ai_accepted_stats(repo, &start_sha, &end_sha, None, ignore_patterns)?;

    // Step 2: Create in-memory authorship log for the range, filtered to only commits in the range
    let commit_shas = commit_range.clone().all_commits();
    let authorship_log =
        create_authorship_log_for_range(repo, &start_sha, &end_sha, &commit_shas, ignore_patterns)?;

    // Step 3: Calculate stats from the authorship log
    let stats = stats_from_authorship_log(
        Some(&authorship_log),
        git_diff_added_lines,
        git_diff_deleted_lines,
        diff_ai_stats.total_ai_accepted,
        &diff_ai_stats.per_tool_model,
    );

    Ok(stats)
}

pub fn print_range_authorship_stats(stats: &RangeAuthorshipStats) {
    println!("\n");

    // If there's no AI authorship in the range, show the special message
    if stats.authorship_stats.commits_with_authorship == 0 {
        println!("Committers are not using git-ai");
        return;
    }

    // Use existing stats terminal output
    use crate::authorship::stats::write_stats_to_terminal;

    // Only print stats if we're in an interactive terminal
    let is_interactive = std::io::stdout().is_terminal();
    write_stats_to_terminal(&stats.range_stats, is_interactive);

    // Check if all individual commits have authorship logs (for optional breakdown)
    let all_have_authorship =
        stats.authorship_stats.commits_with_authorship == stats.authorship_stats.total_commits;

    // If not all commits have authorship logs, show the breakdown
    if !all_have_authorship {
        let commits_without =
            stats.authorship_stats.total_commits - stats.authorship_stats.commits_with_authorship;
        let commit_word = if commits_without == 1 {
            "commit"
        } else {
            "commits"
        };
        println!(
            "  {} {} without Authorship Logs",
            commits_without, commit_word
        );

        // Show each commit without authorship
        for (sha, author) in &stats
            .authorship_stats
            .commits_without_authorship_with_authors
        {
            println!("    {} {}", &sha[0..7], author);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_utils::TmpRepo;

    #[test]
    fn test_range_authorship_simple_range() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create initial commit with human work
        let mut file = tmp_repo.write_file("test.txt", "Line 1\n", true).unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();
        let first_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Add AI work
        file.append("AI Line 2\nAI Line 3\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("AI adds lines").unwrap();
        let second_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Test range authorship from first to second commit
        let commit_range = CommitRange::new(
            tmp_repo.gitai_repo(),
            first_sha.clone(),
            second_sha.clone(),
            "HEAD".to_string(),
        )
        .unwrap();

        let lockfile_patterns = vec![
            "Cargo.lock".to_string(),
            "package-lock.json".to_string(),
            "yarn.lock".to_string(),
        ];
        let stats = range_authorship(commit_range, false, &lockfile_patterns).unwrap();

        // Verify stats
        assert_eq!(stats.authorship_stats.total_commits, 1);
        assert_eq!(stats.authorship_stats.commits_with_authorship, 1);
        assert_eq!(stats.range_stats.ai_additions, 2);
        assert_eq!(stats.range_stats.git_diff_added_lines, 2);
    }

    #[test]
    fn test_range_authorship_from_empty_tree() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create initial commit with AI work
        let mut file = tmp_repo
            .write_file("test.txt", "AI Line 1\n", true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("Initial AI commit").unwrap();

        // Add more AI work
        file.append("AI Line 2\nAI Line 3\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("Second AI commit").unwrap();
        let head_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Test range authorship from empty tree to HEAD
        let commit_range = CommitRange::new(
            tmp_repo.gitai_repo(),
            EMPTY_TREE_HASH.to_string(),
            head_sha.clone(),
            "HEAD".to_string(),
        )
        .unwrap();

        let lockfile_patterns = vec![
            "Cargo.lock".to_string(),
            "package-lock.json".to_string(),
            "yarn.lock".to_string(),
        ];
        let stats = range_authorship(commit_range, false, &lockfile_patterns).unwrap();

        // Verify stats - should include all commits from beginning
        assert_eq!(stats.authorship_stats.total_commits, 2);
        assert_eq!(stats.authorship_stats.commits_with_authorship, 2);
        // When using empty tree, the range stats show the diff from empty to HEAD
        // The AI additions count is based on the filtered attributions for commits in range
        assert_eq!(stats.range_stats.ai_additions, 3);
        assert_eq!(stats.range_stats.git_diff_added_lines, 3);
    }

    #[test]
    fn test_range_authorship_single_commit() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create initial commit
        let mut file = tmp_repo.write_file("test.txt", "Line 1\n", true).unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();

        // Create AI commit
        file.append("AI Line 2\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("AI commit").unwrap();
        let head_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Test range authorship for single commit (start == end)
        let commit_range = CommitRange::new(
            tmp_repo.gitai_repo(),
            head_sha.clone(),
            head_sha.clone(),
            "HEAD".to_string(),
        )
        .unwrap();

        let lockfile_patterns = vec![
            "Cargo.lock".to_string(),
            "package-lock.json".to_string(),
            "yarn.lock".to_string(),
        ];
        let stats = range_authorship(commit_range, false, &lockfile_patterns).unwrap();

        // For single commit, should use stats_for_commit_stats
        assert_eq!(stats.authorship_stats.total_commits, 1);
        assert_eq!(stats.range_stats.ai_additions, 1);
    }

    #[test]
    fn test_range_authorship_mixed_commits() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create initial commit with human work
        let mut file = tmp_repo
            .write_file("test.txt", "Human Line 1\n", true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();
        let first_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Add AI work
        file.append("AI Line 2\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("AI commit").unwrap();

        // Add human work
        file.append("Human Line 3\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Human commit").unwrap();

        // Add more AI work
        file.append("AI Line 4\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("Another AI commit").unwrap();
        let head_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Test range authorship from first to head
        let commit_range = CommitRange::new(
            tmp_repo.gitai_repo(),
            first_sha.clone(),
            head_sha.clone(),
            "HEAD".to_string(),
        )
        .unwrap();

        let lockfile_patterns = vec![
            "Cargo.lock".to_string(),
            "package-lock.json".to_string(),
            "yarn.lock".to_string(),
        ];
        let stats = range_authorship(commit_range, false, &lockfile_patterns).unwrap();

        // Verify stats
        assert_eq!(stats.authorship_stats.total_commits, 3);
        assert_eq!(stats.authorship_stats.commits_with_authorship, 3);
        // Range authorship merges attributions from start to end, filtering to commits in range
        // The exact AI/human split depends on the merge attribution logic
        assert_eq!(stats.range_stats.ai_additions, 2);
        assert_eq!(stats.range_stats.human_additions, 1);
        assert_eq!(stats.range_stats.git_diff_added_lines, 3);
    }

    #[test]
    fn test_range_authorship_no_changes() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create a commit
        tmp_repo.write_file("test.txt", "Line 1\n", true).unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();
        let sha = tmp_repo.get_head_commit_sha().unwrap();

        // Test range authorship with same start and end (already tested above but worth verifying)
        let commit_range = CommitRange::new(
            tmp_repo.gitai_repo(),
            sha.clone(),
            sha.clone(),
            "HEAD".to_string(),
        )
        .unwrap();

        let lockfile_patterns = vec![
            "Cargo.lock".to_string(),
            "package-lock.json".to_string(),
            "yarn.lock".to_string(),
        ];
        let stats = range_authorship(commit_range, false, &lockfile_patterns).unwrap();

        // Should have 1 commit but no diffs since start == end
        assert_eq!(stats.authorship_stats.total_commits, 1);
    }

    #[test]
    fn test_range_authorship_empty_tree_with_multiple_files() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create multiple files with AI work in first commit
        tmp_repo
            .write_file("file1.txt", "AI content 1\n", true)
            .unwrap();
        tmp_repo
            .write_file("file2.txt", "AI content 2\n", true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();
        tmp_repo
            .commit_with_message("Initial multi-file commit")
            .unwrap();
        let head_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Test range authorship from empty tree
        let commit_range = CommitRange::new(
            tmp_repo.gitai_repo(),
            EMPTY_TREE_HASH.to_string(),
            head_sha.clone(),
            "HEAD".to_string(),
        )
        .unwrap();

        let lockfile_patterns = vec![
            "Cargo.lock".to_string(),
            "package-lock.json".to_string(),
            "yarn.lock".to_string(),
        ];
        let stats = range_authorship(commit_range, false, &lockfile_patterns).unwrap();

        // Verify all files are included
        assert_eq!(stats.authorship_stats.total_commits, 1);
        assert_eq!(stats.authorship_stats.commits_with_authorship, 1);
        assert_eq!(stats.range_stats.ai_additions, 2);
        assert_eq!(stats.range_stats.git_diff_added_lines, 2);
    }

    #[test]
    fn test_range_authorship_ignores_single_lockfile() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create initial commit with a source file
        tmp_repo
            .write_file("src/main.rs", "fn main() {}\n", true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();
        let first_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Add AI work to source file and also change a lockfile
        tmp_repo
            .write_file(
                "src/main.rs",
                "fn main() {}\n// AI added code\nfn helper() {}\n",
                true,
            )
            .unwrap();
        tmp_repo
            .write_file(
                "Cargo.lock",
                "# Large lockfile with 1000 lines\n".repeat(1000).as_str(),
                true,
            )
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();
        tmp_repo
            .commit_with_message("Add helper and update deps")
            .unwrap();
        let second_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Test range authorship
        let commit_range = CommitRange::new(
            tmp_repo.gitai_repo(),
            first_sha.clone(),
            second_sha.clone(),
            "HEAD".to_string(),
        )
        .unwrap();

        let lockfile_patterns = vec![
            "Cargo.lock".to_string(),
            "package-lock.json".to_string(),
            "yarn.lock".to_string(),
        ];
        let stats = range_authorship(commit_range, false, &lockfile_patterns).unwrap();

        // Verify lockfile is excluded: only 2 lines added (from main.rs), not 1000+ from lockfile
        assert_eq!(stats.authorship_stats.total_commits, 1);
        assert_eq!(stats.authorship_stats.commits_with_authorship, 1);
        assert_eq!(stats.range_stats.ai_additions, 2); // Only the 2 AI lines in main.rs
        assert_eq!(stats.range_stats.git_diff_added_lines, 2); // Lockfile excluded (1000 lines ignored)
        // The key assertion: git_diff should be 2, not 1002 if lockfile was included
        assert!(stats.range_stats.git_diff_added_lines < 100); // Significantly less than if lockfile was counted
    }

    #[test]
    fn test_range_authorship_mixed_lockfile_and_source() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create initial commit
        tmp_repo
            .write_file("src/lib.rs", "pub fn old() {}\n", true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();
        let first_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Human adds to source file
        tmp_repo
            .write_file("src/lib.rs", "pub fn old() {}\npub fn new() {}\n", true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Human adds function").unwrap();

        // AI adds to source file, and package-lock.json is updated (with 1000 lines)
        tmp_repo
            .write_file(
                "src/lib.rs",
                "pub fn old() {}\npub fn new() {}\n// AI comment\npub fn ai_func() {}\n",
                true,
            )
            .unwrap();
        tmp_repo
            .write_file(
                "package-lock.json",
                "{\n  \"lockfileVersion\": 2,\n}\n".repeat(1000).as_str(),
                true,
            )
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();
        tmp_repo
            .commit_with_message("AI adds function and updates deps")
            .unwrap();
        let head_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Test range authorship
        let commit_range = CommitRange::new(
            tmp_repo.gitai_repo(),
            first_sha.clone(),
            head_sha.clone(),
            "HEAD".to_string(),
        )
        .unwrap();

        let lockfile_patterns = vec![
            "Cargo.lock".to_string(),
            "package-lock.json".to_string(),
            "yarn.lock".to_string(),
        ];
        let stats = range_authorship(commit_range, false, &lockfile_patterns).unwrap();

        // Key assertion: git_diff should only count lib.rs changes (3 lines), not package-lock.json (3000 lines)
        assert_eq!(stats.authorship_stats.total_commits, 2);
        assert_eq!(stats.authorship_stats.commits_with_authorship, 2);
        assert_eq!(stats.range_stats.git_diff_added_lines, 3); // Only lib.rs, package-lock.json excluded
        // Verify the total is much less than 3003 (if lockfile was included)
        assert!(stats.range_stats.git_diff_added_lines < 100);
        // Verify that some AI and human work is detected
        assert!(stats.range_stats.ai_additions > 0);
        assert!(stats.range_stats.human_additions > 0);
    }

    #[test]
    fn test_range_authorship_multiple_lockfile_types() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create initial commit
        tmp_repo
            .write_file("README.md", "# Project\n", true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();
        let first_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Add multiple lockfiles and one real source change
        tmp_repo
            .write_file("Cargo.lock", "# Cargo lock\n".repeat(500).as_str(), true)
            .unwrap();
        tmp_repo
            .write_file("yarn.lock", "# yarn lock\n".repeat(500).as_str(), true)
            .unwrap();
        tmp_repo
            .write_file("poetry.lock", "# poetry lock\n".repeat(500).as_str(), true)
            .unwrap();
        tmp_repo
            .write_file("go.sum", "# go sum\n".repeat(500).as_str(), true)
            .unwrap();
        tmp_repo
            .write_file("README.md", "# Project\n## New Section\n", true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("Update dependencies").unwrap();
        let second_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Test range authorship
        let commit_range = CommitRange::new(
            tmp_repo.gitai_repo(),
            first_sha.clone(),
            second_sha.clone(),
            "HEAD".to_string(),
        )
        .unwrap();

        let lockfile_patterns = vec![
            "Cargo.lock".to_string(),
            "package-lock.json".to_string(),
            "yarn.lock".to_string(),
            "poetry.lock".to_string(),
            "go.sum".to_string(),
        ];
        let stats = range_authorship(commit_range, false, &lockfile_patterns).unwrap();

        // Verify: only the 1 README line is counted, all lockfiles excluded (2000 lines ignored)
        assert_eq!(stats.authorship_stats.total_commits, 1);
        assert_eq!(stats.authorship_stats.commits_with_authorship, 1);
        assert_eq!(stats.range_stats.ai_additions, 1); // Only README.md line
        assert_eq!(stats.range_stats.git_diff_added_lines, 1); // All lockfiles excluded
    }

    #[test]
    fn test_range_authorship_lockfile_only_commit() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create initial commit
        tmp_repo
            .write_file("src/main.rs", "fn main() {}\n", true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();
        let first_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Commit that only changes lockfiles (common scenario)
        tmp_repo
            .write_file(
                "package-lock.json",
                "{\n  \"version\": \"1.0.0\"\n}\n".repeat(1000).as_str(),
                true,
            )
            .unwrap();
        tmp_repo
            .write_file("yarn.lock", "# yarn\n".repeat(500).as_str(), true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo
            .commit_with_message("Update lockfiles only")
            .unwrap();
        let second_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Test range authorship
        let commit_range = CommitRange::new(
            tmp_repo.gitai_repo(),
            first_sha.clone(),
            second_sha.clone(),
            "HEAD".to_string(),
        )
        .unwrap();

        let lockfile_patterns = vec![
            "Cargo.lock".to_string(),
            "package-lock.json".to_string(),
            "yarn.lock".to_string(),
        ];
        let stats = range_authorship(commit_range, false, &lockfile_patterns).unwrap();

        // Verify: no lines counted since only lockfiles changed
        assert_eq!(stats.authorship_stats.total_commits, 1);
        assert_eq!(stats.range_stats.git_diff_added_lines, 0); // All lockfiles excluded
        assert_eq!(stats.range_stats.ai_additions, 0);
        assert_eq!(stats.range_stats.human_additions, 0);
    }

    #[test]
    fn test_should_ignore_file_with_patterns() {
        let lockfile_patterns = vec![
            "package-lock.json".to_string(),
            "yarn.lock".to_string(),
            "Cargo.lock".to_string(),
            "go.sum".to_string(),
        ];

        // Test that specified patterns are ignored
        assert!(should_ignore_file("package-lock.json", &lockfile_patterns));
        assert!(should_ignore_file("yarn.lock", &lockfile_patterns));
        assert!(should_ignore_file("Cargo.lock", &lockfile_patterns));
        assert!(should_ignore_file("go.sum", &lockfile_patterns));

        // Test with paths
        assert!(should_ignore_file(
            "src/package-lock.json",
            &lockfile_patterns
        ));
        assert!(should_ignore_file("backend/Cargo.lock", &lockfile_patterns));
        assert!(should_ignore_file("./yarn.lock", &lockfile_patterns));

        // Test that non-matching files are not ignored
        assert!(!should_ignore_file("package.json", &lockfile_patterns));
        assert!(!should_ignore_file("Cargo.toml", &lockfile_patterns));
        assert!(!should_ignore_file("src/main.rs", &lockfile_patterns));
        assert!(!should_ignore_file("pnpm-lock.yaml", &lockfile_patterns)); // Not in our pattern list

        // Test with empty patterns - nothing should be ignored
        let empty_patterns: Vec<String> = vec![];
        assert!(!should_ignore_file("package-lock.json", &empty_patterns));
        assert!(!should_ignore_file("Cargo.lock", &empty_patterns));
    }

    #[test]
    fn test_should_ignore_file_with_glob_patterns() {
        // Test wildcard patterns
        let wildcard_patterns = vec!["*.lock".to_string()];

        // Should match any file ending in .lock
        assert!(should_ignore_file("Cargo.lock", &wildcard_patterns));
        assert!(should_ignore_file("package.lock", &wildcard_patterns));
        assert!(should_ignore_file("yarn.lock", &wildcard_patterns));
        assert!(should_ignore_file("src/Cargo.lock", &wildcard_patterns));
        assert!(should_ignore_file("backend/deps.lock", &wildcard_patterns));

        // Should not match files not ending in .lock
        assert!(!should_ignore_file("Cargo.toml", &wildcard_patterns));
        assert!(!should_ignore_file("lock.txt", &wildcard_patterns));
        assert!(!should_ignore_file("locked.rs", &wildcard_patterns));

        // Test multiple wildcards
        let multi_wildcard = vec!["*.lock".to_string(), "*.generated.*".to_string()];
        assert!(should_ignore_file("test.generated.js", &multi_wildcard));
        assert!(should_ignore_file("api.generated.ts", &multi_wildcard));
        assert!(should_ignore_file("schema.lock", &multi_wildcard));
        assert!(!should_ignore_file("manual.js", &multi_wildcard));
    }

    #[test]
    fn test_should_ignore_file_with_path_glob_patterns() {
        // Test path-based patterns
        let path_patterns = vec!["**/target/**".to_string()];

        // Should match files in target directory at any depth
        assert!(should_ignore_file("target/debug/foo", &path_patterns));
        assert!(should_ignore_file(
            "backend/target/release/bar",
            &path_patterns
        ));
        assert!(should_ignore_file("project/target/file.rs", &path_patterns));

        // Should not match files outside target
        assert!(!should_ignore_file("src/target.rs", &path_patterns));
        assert!(!should_ignore_file("target.txt", &path_patterns));

        // Test specific directory patterns
        let dir_patterns = vec!["node_modules/**".to_string()];
        assert!(should_ignore_file(
            "node_modules/package/index.js",
            &dir_patterns
        ));
        assert!(should_ignore_file("node_modules/foo.js", &dir_patterns));
        assert!(!should_ignore_file("src/node_modules.rs", &dir_patterns));
    }

    #[test]
    fn test_should_ignore_file_with_prefix_patterns() {
        // Test prefix patterns
        let prefix_patterns = vec!["generated-*".to_string()];

        assert!(should_ignore_file("generated-api.ts", &prefix_patterns));
        assert!(should_ignore_file("generated-schema.js", &prefix_patterns));
        assert!(should_ignore_file(
            "src/generated-types.d.ts",
            &prefix_patterns
        ));
        assert!(!should_ignore_file("api-generated.ts", &prefix_patterns));
        assert!(!should_ignore_file("manual.ts", &prefix_patterns));
    }

    #[test]
    fn test_should_ignore_file_with_complex_glob_patterns() {
        // Test complex patterns (note: brace expansion like {js,ts} is not supported by glob crate)
        let complex_patterns = vec![
            "**/*.generated.js".to_string(),
            "**/*.generated.ts".to_string(),
            "*-lock.*".to_string(),
            "dist/**".to_string(),
        ];

        // Glob patterns with multiple wildcards
        assert!(should_ignore_file(
            "src/api.generated.js",
            &complex_patterns
        ));
        assert!(should_ignore_file("types.generated.ts", &complex_patterns));
        assert!(should_ignore_file("package-lock.json", &complex_patterns));
        assert!(should_ignore_file("yarn-lock.yaml", &complex_patterns));
        assert!(should_ignore_file("dist/bundle.js", &complex_patterns));
        assert!(should_ignore_file(
            "dist/nested/file.css",
            &complex_patterns
        ));

        assert!(!should_ignore_file("src/manual.js", &complex_patterns));
        assert!(!should_ignore_file("lock.txt", &complex_patterns));
    }

    #[test]
    fn test_should_ignore_file_mixed_exact_and_glob() {
        // Test mixing exact matches and glob patterns
        let mixed_patterns = vec![
            "Cargo.lock".to_string(),        // Exact match
            "*.generated.js".to_string(),    // Glob pattern
            "package-lock.json".to_string(), // Exact match
            "**/target/**".to_string(),      // Path glob
        ];

        // Exact matches
        assert!(should_ignore_file("Cargo.lock", &mixed_patterns));
        assert!(should_ignore_file("package-lock.json", &mixed_patterns));

        // Glob matches
        assert!(should_ignore_file("api.generated.js", &mixed_patterns));
        assert!(should_ignore_file("target/debug/foo", &mixed_patterns));

        // Non-matches
        assert!(!should_ignore_file("Cargo.toml", &mixed_patterns));
        assert!(!should_ignore_file("manual.js", &mixed_patterns));
    }

    #[test]
    fn test_should_ignore_file_case_sensitivity() {
        // Test that pattern matching is case-sensitive
        let patterns = vec!["Cargo.lock".to_string(), "*.LOG".to_string()];

        // Exact case matches
        assert!(should_ignore_file("Cargo.lock", &patterns));
        assert!(should_ignore_file("file.LOG", &patterns));
        assert!(should_ignore_file("debug.LOG", &patterns));

        // Different case should NOT match (case-sensitive)
        assert!(!should_ignore_file("cargo.lock", &patterns));
        assert!(!should_ignore_file("CARGO.LOCK", &patterns));
        assert!(!should_ignore_file("file.log", &patterns));
        assert!(!should_ignore_file("file.Log", &patterns));
    }

    #[test]
    fn test_should_ignore_file_special_characters() {
        // Test filenames with special characters
        let patterns = vec![
            "file with spaces.txt".to_string(),
            "*.lock".to_string(),
            "file-with-dashes.js".to_string(),
            "file_with_underscores.rs".to_string(),
        ];

        // Files with spaces
        assert!(should_ignore_file("file with spaces.txt", &patterns));
        assert!(should_ignore_file(
            "path/to/file with spaces.txt",
            &patterns
        ));

        // Files with dashes and underscores
        assert!(should_ignore_file("file-with-dashes.js", &patterns));
        assert!(should_ignore_file("file_with_underscores.rs", &patterns));

        // Glob should still work with special chars in other files
        assert!(should_ignore_file("my-package.lock", &patterns));
        assert!(should_ignore_file("test_file.lock", &patterns));

        // Non-matches
        assert!(!should_ignore_file("file with spaces.js", &patterns));
        assert!(!should_ignore_file("different-file.txt", &patterns));
    }

    #[test]
    fn test_should_ignore_file_hidden_files() {
        // Test hidden files (starting with .)
        let patterns = vec![".env".to_string(), ".*.swp".to_string(), ".*rc".to_string()];

        // Hidden files
        assert!(should_ignore_file(".env", &patterns));
        assert!(should_ignore_file("config/.env", &patterns));

        // Vim swap files
        assert!(should_ignore_file(".file.swp", &patterns));
        assert!(should_ignore_file(".main.rs.swp", &patterns));

        // RC files
        assert!(should_ignore_file(".bashrc", &patterns));
        assert!(should_ignore_file(".vimrc", &patterns));
        assert!(should_ignore_file("home/.npmrc", &patterns));

        // Non-matches
        assert!(!should_ignore_file("env", &patterns));
        assert!(!should_ignore_file("file.swp", &patterns));
        assert!(!should_ignore_file("bashrc", &patterns));
    }

    #[test]
    fn test_should_ignore_file_multiple_extensions() {
        // Test files with multiple extensions
        let patterns = vec![
            "*.tar.gz".to_string(),
            "*.min.js".to_string(),
            "*.d.ts".to_string(),
        ];

        // Multiple extensions
        assert!(should_ignore_file("archive.tar.gz", &patterns));
        assert!(should_ignore_file("bundle.min.js", &patterns));
        assert!(should_ignore_file("types.d.ts", &patterns));
        assert!(should_ignore_file("build/dist/app.min.js", &patterns));

        // Partial matches should not match
        assert!(!should_ignore_file("file.tar", &patterns));
        assert!(!should_ignore_file("file.gz", &patterns));
        assert!(!should_ignore_file("file.js", &patterns));
        assert!(!should_ignore_file("types.ts", &patterns));
    }

    #[test]
    fn test_should_ignore_file_no_extension() {
        // Test files without extensions
        let patterns = vec![
            "Makefile".to_string(),
            "Dockerfile".to_string(),
            "LICENSE".to_string(),
            "README".to_string(),
        ];

        // Files without extensions
        assert!(should_ignore_file("Makefile", &patterns));
        assert!(should_ignore_file("Dockerfile", &patterns));
        assert!(should_ignore_file("LICENSE", &patterns));
        assert!(should_ignore_file("README", &patterns));

        // In subdirectories
        assert!(should_ignore_file("project/Makefile", &patterns));
        assert!(should_ignore_file("docker/Dockerfile", &patterns));

        // Similar names should not match
        assert!(!should_ignore_file("Makefile.old", &patterns));
        assert!(!should_ignore_file("README.md", &patterns));
        assert!(!should_ignore_file("LICENSE.txt", &patterns));
    }

    #[test]
    fn test_should_ignore_file_deeply_nested_paths() {
        // Test patterns at various nesting depths
        let patterns = vec![
            "**/node_modules/**".to_string(),
            "**/build/**".to_string(),
            "**/.git/**".to_string(),
        ];

        // Deep nesting
        assert!(should_ignore_file(
            "node_modules/package/index.js",
            &patterns
        ));
        assert!(should_ignore_file("a/b/c/node_modules/d/e/f.js", &patterns));
        assert!(should_ignore_file(
            "project/build/output/bundle.js",
            &patterns
        ));
        assert!(should_ignore_file(".git/objects/ab/cdef123", &patterns));
        assert!(should_ignore_file("repo/.git/hooks/pre-commit", &patterns));

        // Should not match similar names outside pattern
        assert!(!should_ignore_file("src/node_modules.js", &patterns));
        assert!(!should_ignore_file("build.sh", &patterns));
        assert!(!should_ignore_file("git.txt", &patterns));
    }

    #[test]
    fn test_should_ignore_file_partial_matches() {
        // Test that partial matches don't incorrectly match
        let patterns = vec!["lock".to_string(), "*.lock".to_string()];

        // Should match
        assert!(should_ignore_file("lock", &patterns));
        assert!(should_ignore_file("file.lock", &patterns));
        assert!(should_ignore_file("package.lock", &patterns));

        // Should NOT match (lock is substring but not filename or extension)
        assert!(!should_ignore_file("locked.txt", &patterns));
        assert!(!should_ignore_file("unlock.sh", &patterns));
        assert!(!should_ignore_file("locksmith.rs", &patterns));
    }

    #[test]
    fn test_should_ignore_file_with_wildcards_in_middle() {
        // Test patterns with wildcards in the middle
        let patterns = vec!["test-*-output.log".to_string(), "backup-*.sql".to_string()];

        // Should match
        assert!(should_ignore_file("test-123-output.log", &patterns));
        assert!(should_ignore_file("test-foo-output.log", &patterns));
        assert!(should_ignore_file("backup-daily.sql", &patterns));
        assert!(should_ignore_file("backup-2024-01-01.sql", &patterns));
        assert!(should_ignore_file("logs/test-debug-output.log", &patterns));

        // Should not match
        assert!(!should_ignore_file("test-output.log", &patterns));
        assert!(!should_ignore_file("test-123-result.log", &patterns));
        assert!(!should_ignore_file("backup.sql", &patterns));
    }

    #[test]
    fn test_should_ignore_file_empty_pattern() {
        // Test with empty pattern string - empty pattern is technically valid glob
        // that matches empty string, but we test that non-empty files don't match
        let patterns = vec!["".to_string(), "*.lock".to_string()];

        // Regular files should not match the empty pattern
        assert!(!should_ignore_file("file.txt", &patterns));
        assert!(!should_ignore_file("src/main.rs", &patterns));

        // But valid patterns should still work
        assert!(should_ignore_file("file.lock", &patterns));
        assert!(should_ignore_file("package.lock", &patterns));
    }

    #[test]
    fn test_should_ignore_file_directory_traversal() {
        // Test patterns with ../ or ./ in paths
        let patterns = vec!["*.lock".to_string()];

        // Should match regardless of ./ prefix
        assert!(should_ignore_file("./file.lock", &patterns));
        assert!(should_ignore_file("./path/to/file.lock", &patterns));

        // Complex paths
        assert!(should_ignore_file("src/../lib/file.lock", &patterns));
    }

    #[test]
    fn test_should_ignore_file_numeric_filenames() {
        // Test numeric filenames
        let patterns = vec!["[0-9]*".to_string(), "*.123".to_string()];

        // Filenames starting with numbers
        assert!(should_ignore_file("123.txt", &patterns));
        assert!(should_ignore_file("456file.log", &patterns));
        assert!(should_ignore_file("7890.rs", &patterns));

        // Files ending with .123
        assert!(should_ignore_file("backup.123", &patterns));
        assert!(should_ignore_file("data.123", &patterns));

        // Should not match
        assert!(!should_ignore_file("file123.txt", &patterns));
        assert!(!should_ignore_file("test.456", &patterns));
    }

    #[test]
    fn test_range_authorship_with_glob_patterns() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Initial commit
        tmp_repo
            .write_file("src/main.rs", "fn main() {}\n", true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();
        let first_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Add various files including lockfiles and generated files
        tmp_repo
            .write_file("src/main.rs", "fn main() {}\nfn helper() {}\n", true)
            .unwrap();
        tmp_repo
            .write_file("Cargo.lock", "# lock\n".repeat(1000).as_str(), true)
            .unwrap();
        tmp_repo
            .write_file("package-lock.json", "{}\n".repeat(500).as_str(), true)
            .unwrap();
        tmp_repo
            .write_file(
                "api.generated.js",
                "// generated\n".repeat(200).as_str(),
                true,
            )
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("Add code and deps").unwrap();
        let second_sha = tmp_repo.get_head_commit_sha().unwrap();

        let commit_range = CommitRange::new(
            tmp_repo.gitai_repo(),
            first_sha.clone(),
            second_sha.clone(),
            "HEAD".to_string(),
        )
        .unwrap();

        // Use glob patterns to ignore lockfiles and generated files
        let glob_patterns = vec![
            "*.lock".to_string(),
            "*lock.json".to_string(), // Matches package-lock.json
            "*.generated.*".to_string(),
        ];
        let stats = range_authorship(commit_range, false, &glob_patterns).unwrap();

        // Should only count the 1 line in main.rs, ignoring 1700 lines in lockfiles and generated files
        assert_eq!(stats.range_stats.git_diff_added_lines, 1);
        assert_eq!(stats.range_stats.ai_additions, 1);
    }
}
