use std::collections::HashMap;
use std::collections::HashSet;

use serde::Deserialize;
use serde::Serialize;

use crate::authorship::stats::{CommitStats, stats_for_commit_stats, stats_from_authorship_log};
use crate::error::GitAiError;
use crate::git::refs::{CommitAuthorship, get_commits_with_notes_from_list};
use crate::git::repository::{CommitRange, Repository};
use crate::utils::debug_log;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeAuthorshipStats {
    pub authorship_stats: RangeAuthorshipStatsData,
    pub range_stats: CommitStats,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeAuthorshipStatsData {
    pub total_commits: usize,
    pub commits_with_authorship: usize,
    pub authors_commiting_authorship: HashSet<String>,
    pub authors_not_commiting_authorship: HashSet<String>,
    pub commits_without_authorship: Vec<String>,
    pub commits_without_authorship_with_authors: Vec<(String, String)>, // (sha, git_author)
}

pub fn range_authorship(
    commit_range: CommitRange,
    pre_fetch_contents: bool,
) -> Result<RangeAuthorshipStats, GitAiError> {
    if let Err(e) = commit_range.is_valid() {
        return Err(e);
    }

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
    let range_stats = calculate_range_stats_direct(repository, commit_range_clone)?;

    Ok(RangeAuthorshipStats {
        authorship_stats: RangeAuthorshipStatsData {
            total_commits: commit_authorship.len(),
            commits_with_authorship: commit_authorship
                .iter()
                .filter(|ca| matches!(ca, CommitAuthorship::Log { .. }))
                .count(),
            authors_commiting_authorship: commit_authorship
                .iter()
                .filter_map(|ca| match ca {
                    CommitAuthorship::Log { git_author, .. } => Some(git_author.clone()),
                    _ => None,
                })
                .collect(),
            authors_not_commiting_authorship: commit_authorship
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
) -> Result<crate::authorship::authorship_log_serialization::AuthorshipLog, GitAiError> {
    use crate::authorship::virtual_attribution::{
        VirtualAttributions, merge_attributions_favoring_first,
    };

    debug_log(&format!(
        "Calculating authorship log for range: {} -> {}",
        start_sha, end_sha
    ));

    // Step 1: Get list of changed files between the two commits
    let changed_files = repo.diff_changed_files(start_sha, end_sha)?;

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

    // Step 2: Create VirtualAttributions for start commit (older)
    let repo_clone = repo.clone();
    let mut start_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(repo_clone, start_sha.to_string(), &changed_files, None)
            .await
    })?;

    // Step 3: Create VirtualAttributions for end commit (newer)
    let repo_clone = repo.clone();
    let mut end_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(repo_clone, end_sha.to_string(), &changed_files, None)
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
) -> Result<(u32, u32), GitAiError> {
    // Use git diff --numstat to get diff statistics for the range
    let mut args = repo.global_args_for_exec();
    args.push("diff".to_string());
    args.push("--numstat".to_string());
    args.push(format!("{}..{}", start_sha, end_sha));

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)?;

    let mut added_lines = 0u32;
    let mut deleted_lines = 0u32;

    // Parse numstat output
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // Parse numstat format: "added\tdeleted\tfilename"
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 2 {
            // Parse added lines
            if let Ok(added) = parts[0].parse::<u32>() {
                added_lines += added;
            }

            // Parse deleted lines (handle "-" for binary files)
            if parts[1] != "-" {
                if let Ok(deleted) = parts[1].parse::<u32>() {
                    deleted_lines += deleted;
                }
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
) -> Result<CommitStats, GitAiError> {
    let start_sha = commit_range.start_oid.clone();
    let end_sha = commit_range.end_oid.clone();
    // Special case: single commit range (start == end)
    if start_sha == end_sha {
        return stats_for_commit_stats(repo, &end_sha, &commit_range.refname);
    }

    // Step 1: Get git diff stats between start and end
    let (git_diff_added_lines, git_diff_deleted_lines) =
        get_git_diff_stats_for_range(repo, &start_sha, &end_sha)?;

    // Step 2: Create in-memory authorship log for the range, filtered to only commits in the range
    let commit_shas = commit_range.clone().all_commits();
    let authorship_log = create_authorship_log_for_range(repo, &start_sha, &end_sha, &commit_shas)?;

    // Step 3: Calculate stats from the authorship log
    let stats = stats_from_authorship_log(
        Some(&authorship_log),
        git_diff_added_lines,
        git_diff_deleted_lines,
    );

    Ok(stats)
}

pub fn print_range_authorship_stats(stats: &RangeAuthorshipStats) {
    println!("\n");

    // Check if there's any AI authorship in the range (based on the in-memory squashed authorship log)
    let has_ai_authorship =
        stats.range_stats.ai_additions > 0 || stats.range_stats.total_ai_additions > 0;

    // If there's no AI authorship in the range, show the special message
    if !has_ai_authorship {
        println!("Committers are not using git-ai");
        return;
    }

    // Use existing stats terminal output
    use crate::authorship::stats::write_stats_to_terminal;
    write_stats_to_terminal(&stats.range_stats, true);

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
