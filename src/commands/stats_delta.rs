use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

use crate::authorship::post_commit::filter_untracked_files;
use crate::authorship::stats::{
    CommitStats, get_git_diff_stats, stats_for_commit_stats, stats_from_authorship_log,
};
use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::error::GitAiError;
use crate::git::refs::get_authorship;
use crate::git::repository::{Repository, exec_git};

pub fn handle_stats_delta(repository_option: &Option<Repository>, _args: &[String]) {
    if repository_option.is_none() {
        eprintln!("No repository found from current directory");
        std::process::exit(1);
    }
    let repository = repository_option.as_ref().unwrap();

    let stats_delta_log_path = repository
        .storage
        .repo_path
        .join("ai")
        .join("stats_delta.log");

    let mut stats_delta_log = StatsDeltaLog::new(stats_delta_log_path).unwrap();

    // Collect new landed entries to print at the end
    let mut new_landed_entries: Vec<StatsDeltaLogEntry> = Vec::new();

    // Step 1: Get HEAD commit SHA
    let head_commit = match repository.head().and_then(|h| h.peel_to_commit()) {
        Ok(commit) => commit,
        Err(e) => {
            eprintln!("Failed to get HEAD commit: {}", e);
            std::process::exit(1);
        }
    };
    let head_sha = head_commit.id();

    // Step 2: Determine commit range to process
    let range_spec = if let Some(last_indexed) = stats_delta_log.last_indexed_commit() {
        // Try to verify the last indexed commit still exists
        match repository.find_commit(last_indexed.to_string()) {
            Ok(_) => format!("{}..{}", last_indexed, head_sha),
            Err(_) => {
                // Commit doesn't exist anymore (squashed/rebased), fall back to HEAD~1..HEAD
                format!("{}~1..{}", head_sha, head_sha)
            }
        }
    } else {
        // Initial run, use HEAD~1..HEAD
        format!("{}~1..{}", head_sha, head_sha)
    };

    // Get commits using git rev-list
    let mut args_list = repository.global_args_for_exec();
    args_list.push("rev-list".to_string());
    args_list.push("--max-count=50".to_string());
    args_list.push(range_spec.clone());

    let commit_shas = match exec_git(&args_list) {
        Ok(output) => {
            let stdout = String::from_utf8(output.stdout).unwrap_or_default();
            stdout
                .lines()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
        }
        Err(e) => {
            eprintln!("Failed to get commit list: {}", e);
            Vec::new()
        }
    };

    // Step 3: Check for active editing at HEAD
    let head_working_log_dir = repository.storage.working_logs.join(&head_sha);
    if head_working_log_dir.exists() {
        // Check if we already have an Editing entry for this SHA
        if stats_delta_log.find_by_sha(&head_sha).is_some() {
            // Touch to update last_seen
            stats_delta_log.touch(&head_sha);
        } else {
            // Create new Editing entry
            let now = Utc::now();
            stats_delta_log.add(StatsDeltaLogEntry::Editing {
                working_log_base_sha: head_sha.clone(),
                first_seen: now,
                last_seen: now,
            });
        }
    }

    // Step 4: Process each commit from rev-list
    for commit_sha in commit_shas {
        let commit = match repository.find_commit(commit_sha.clone()) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Check if we already have an entry for this commit
        if stats_delta_log.find_by_sha(&commit_sha).is_some() {
            continue; // Skip if already processed
        }

        // a) First, check for LandedGitAIPostCommit (priority)
        if let Some(_authorship_log) = get_authorship(repository, &commit_sha) {
            // Has authorship notes - this is a git-ai post-commit
            if let Ok(parent) = commit.parent(0) {
                let parent_sha = parent.id();

                // Remove any Editing entry for this parent SHA (consolidation)
                stats_delta_log.delete(&parent_sha);

                let stats = stats_for_commit_stats(repository, &commit_sha).unwrap_or_else(|e| {
                    eprintln!("Failed to compute stats for commit {}: {}", commit_sha, e);
                    CommitStats::default()
                });

                let entry = StatsDeltaLogEntry::LandedGitAIPostCommit {
                    working_log_base_sha: parent_sha,
                    commit_sha: commit_sha.clone(),
                    stats,
                    processed_at: Utc::now(),
                };

                new_landed_entries.push(entry.clone());
                stats_delta_log.add(entry);
            }
            continue; // Skip heuristic check
        }

        // b) Otherwise, check for LandedCommitHueristic (fallback)
        if let Ok(parent_count) = commit.parent_count() {
            if parent_count == 1 {
                if let Ok(parent) = commit.parent(0) {
                    let parent_sha = parent.id();
                    let parent_working_log_dir = repository.storage.working_logs.join(&parent_sha);

                    if parent_working_log_dir.exists() {
                        // Remove any Editing entry for this parent SHA (consolidation)
                        stats_delta_log.delete(&parent_sha);

                        let stats = simulate_post_commit(repository, &parent_sha, &commit_sha)
                            .unwrap_or_else(|e| {
                                eprintln!(
                                    "Failed to simulate stats for commit {}: {}",
                                    commit_sha, e
                                );
                                CommitStats::default()
                            });

                        // This commit has a working log parent - mark as heuristic
                        let entry = StatsDeltaLogEntry::LandedCommitHueristic {
                            working_log_base_sha: parent_sha,
                            stats,
                            processed_at: Utc::now(),
                        };

                        new_landed_entries.push(entry.clone());
                        stats_delta_log.add(entry);
                    }
                }
            }
        }
    }

    // Step 5: Update tracking metadata
    stats_delta_log.set_last_indexed_commit(head_sha);

    // Step 6: Save the log
    if let Err(e) = stats_delta_log.save() {
        eprintln!("Failed to save stats_delta log: {}", e);
        std::process::exit(1);
    }

    // Print new landed entries as JSON
    match serde_json::to_string_pretty(&new_landed_entries) {
        Ok(json) => println!("{}", json),
        Err(e) => eprintln!("Failed to serialize new landed entries: {}", e),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StatsDeltaLogEntry {
    Editing {
        working_log_base_sha: String,
        first_seen: DateTime<Utc>,
        last_seen: DateTime<Utc>,
    },
    LandedCommitHueristic {
        working_log_base_sha: String,
        stats: CommitStats,
        processed_at: DateTime<Utc>,
    },
    LandedGitAIPostCommit {
        working_log_base_sha: String, // parent sha
        commit_sha: String,           // commit sha
        stats: CommitStats,
        processed_at: DateTime<Utc>,
    },
}

impl StatsDeltaLogEntry {
    /// Get the working_log_base_sha for this entry (common across all variants)
    pub fn working_log_base_sha(&self) -> &str {
        match self {
            StatsDeltaLogEntry::Editing {
                working_log_base_sha,
                ..
            } => working_log_base_sha,
            StatsDeltaLogEntry::LandedCommitHueristic {
                working_log_base_sha,
                ..
            } => working_log_base_sha,
            StatsDeltaLogEntry::LandedGitAIPostCommit {
                working_log_base_sha,
                ..
            } => working_log_base_sha,
        }
    }

    /// Touch the last_seen time for Editing entries (returns new entry)
    pub fn touch(&self) -> Self {
        match self {
            StatsDeltaLogEntry::Editing {
                working_log_base_sha,
                first_seen,
                ..
            } => StatsDeltaLogEntry::Editing {
                working_log_base_sha: working_log_base_sha.clone(),
                first_seen: *first_seen,
                last_seen: Utc::now(),
            },
            _ => self.clone(),
        }
    }
}

/// The on-disk format for the stats delta log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsDeltaLogData {
    pub last_indexed_commit: Option<String>,
    pub last_indexed_timestamp: Option<DateTime<Utc>>,
    pub log: Vec<StatsDeltaLogEntry>,
}

/// Manages a collection of StatsDeltaLogEntry items with load/save capabilities
///
/// # Example Usage
/// ```no_run
/// use chrono::Utc;
/// use std::path::PathBuf;
///
/// // Auto-loads from disk if file exists, otherwise creates new
/// let mut log = StatsDeltaLog::new(PathBuf::from("stats_delta.log")).unwrap();
///
/// // Update metadata
/// log.set_last_indexed_commit("abc123".to_string());
///
/// // Add a new entry
/// log.add(StatsDeltaLogEntry::Editing {
///     working_log_base_sha: "abc123".to_string(),
///     first_seen: Utc::now(),
///     last_seen: Utc::now(),
/// });
///
/// // Touch an existing entry to update last_seen
/// log.touch("abc123");
///
/// // Replace an entry (state transition)
/// log.replace(StatsDeltaLogEntry::LandedCommitHueristic {
///     working_log_base_sha: "abc123".to_string(),
///     stats: Default::default(),
///     processed_at: Utc::now(),
/// });
///
/// // Delete an entry
/// log.delete("abc123");
///
/// // Save changes
/// log.save().unwrap();
/// ```
pub struct StatsDeltaLog {
    path: PathBuf,
    last_indexed_commit: Option<String>,
    last_indexed_timestamp: Option<DateTime<Utc>>,
    entries: Vec<StatsDeltaLogEntry>,
}

impl StatsDeltaLog {
    /// Create a new StatsDeltaLog with the given path
    ///
    /// If the file exists on disk, it will be automatically loaded.
    /// Otherwise, a fresh empty log is created.
    pub fn new(path: PathBuf) -> Result<Self, std::io::Error> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self {
                path,
                last_indexed_commit: None,
                last_indexed_timestamp: None,
                entries: Vec::new(),
            })
        }
    }

    /// Load the log from disk (JSON format)
    fn load(path: PathBuf) -> Result<Self, std::io::Error> {
        let mut file = File::open(&path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        let data: StatsDeltaLogData = serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        Ok(Self {
            path,
            last_indexed_commit: data.last_indexed_commit,
            last_indexed_timestamp: data.last_indexed_timestamp,
            entries: data.log,
        })
    }

    /// Save the log to disk (JSON format)
    pub fn save(&self) -> Result<(), std::io::Error> {
        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let data = StatsDeltaLogData {
            last_indexed_commit: self.last_indexed_commit.clone(),
            last_indexed_timestamp: self.last_indexed_timestamp,
            log: self.entries.clone(),
        };

        let json = serde_json::to_string_pretty(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let mut file = File::create(&self.path)?;
        file.write_all(json.as_bytes())?;

        Ok(())
    }

    /// Get the last indexed commit SHA
    pub fn last_indexed_commit(&self) -> Option<&str> {
        self.last_indexed_commit.as_deref()
    }

    /// Get the last indexed timestamp
    pub fn last_indexed_timestamp(&self) -> Option<DateTime<Utc>> {
        self.last_indexed_timestamp
    }

    /// Set the last indexed commit SHA and update timestamp to now
    pub fn set_last_indexed_commit(&mut self, commit_sha: String) {
        self.last_indexed_commit = Some(commit_sha);
        self.last_indexed_timestamp = Some(Utc::now());
    }

    /// Set the last indexed commit SHA with a specific timestamp
    pub fn set_last_indexed_commit_at(&mut self, commit_sha: String, timestamp: DateTime<Utc>) {
        self.last_indexed_commit = Some(commit_sha);
        self.last_indexed_timestamp = Some(timestamp);
    }

    /// Get all entries
    pub fn entries(&self) -> &[StatsDeltaLogEntry] {
        &self.entries
    }

    /// Get mutable access to all entries
    pub fn entries_mut(&mut self) -> &mut Vec<StatsDeltaLogEntry> {
        &mut self.entries
    }

    /// Find an entry by working_log_base_sha
    pub fn find_by_sha(&self, working_log_base_sha: &str) -> Option<&StatsDeltaLogEntry> {
        self.entries
            .iter()
            .find(|e| e.working_log_base_sha() == working_log_base_sha)
    }

    /// Find a mutable entry by working_log_base_sha
    pub fn find_by_sha_mut(
        &mut self,
        working_log_base_sha: &str,
    ) -> Option<&mut StatsDeltaLogEntry> {
        self.entries
            .iter_mut()
            .find(|e| e.working_log_base_sha() == working_log_base_sha)
    }

    /// Add a new entry to the log
    pub fn add(&mut self, entry: StatsDeltaLogEntry) {
        self.entries.push(entry);
    }

    /// Replace an entry by working_log_base_sha
    pub fn replace(&mut self, new_entry: StatsDeltaLogEntry) -> bool {
        let sha = new_entry.working_log_base_sha().to_string();
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.working_log_base_sha() == sha)
        {
            self.entries[pos] = new_entry;
            true
        } else {
            false
        }
    }

    /// Replace an entry by working_log_base_sha, or add it if it doesn't exist
    pub fn upsert(&mut self, entry: StatsDeltaLogEntry) {
        if !self.replace(entry.clone()) {
            self.add(entry);
        }
    }

    /// Delete an entry by working_log_base_sha
    pub fn delete(&mut self, working_log_base_sha: &str) -> bool {
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.working_log_base_sha() == working_log_base_sha)
        {
            self.entries.remove(pos);
            true
        } else {
            false
        }
    }

    /// Touch the last_seen time for an Editing entry by working_log_base_sha
    pub fn touch(&mut self, working_log_base_sha: &str) -> bool {
        if let Some(entry) = self.find_by_sha_mut(working_log_base_sha) {
            *entry = entry.touch();
            true
        } else {
            false
        }
    }

    /// Delete all entries matching a predicate
    pub fn delete_where<F>(&mut self, predicate: F) -> usize
    where
        F: Fn(&StatsDeltaLogEntry) -> bool,
    {
        let initial_len = self.entries.len();
        self.entries.retain(|e| !predicate(e));
        initial_len - self.entries.len()
    }
}

/// Simulate post_commit stats generation from a working log (read-only)
///
/// This is used for the LandedCommitHueristic case where we have a working log
/// but the commit was made without git-ai post-commit. We simulate what the stats
/// would have been if post_commit had run.
fn simulate_post_commit(
    repository: &Repository,
    working_log_base_sha: &str,
    commit_sha: &str,
) -> Result<CommitStats, GitAiError> {
    // Check if working log directory exists
    let working_log_dir = repository.storage.working_logs.join(working_log_base_sha);
    if !working_log_dir.exists() {
        return Err(GitAiError::Generic(format!(
            "Working log directory does not exist for base SHA: {}",
            working_log_base_sha
        )));
    }

    // Read working log checkpoints for the base SHA
    let working_log = repository
        .storage
        .working_log_for_base_commit(working_log_base_sha);
    let parent_working_log = working_log.read_all_checkpoints()?;

    // Filter out untracked files from the working log
    let filtered_working_log =
        filter_untracked_files(repository, &parent_working_log, commit_sha, None)?;

    // Create VirtualAttributions from working log with stubbed human author
    let working_va = VirtualAttributions::from_just_working_log(
        repository.clone(),
        working_log_base_sha.to_string(),
        Some("example@usegitai.com".to_string()),
    )?;

    // Get pathspecs for files in the working log
    let pathspecs: HashSet<String> = filtered_working_log
        .iter()
        .flat_map(|cp| cp.entries.iter().map(|e| e.file.clone()))
        .collect();

    // Convert VirtualAttributions to authorship log (index-only mode)
    // This only looks at committed hunks, not the working copy, since the commit has already landed
    let authorship_log = working_va.to_authorship_log_index_only(
        repository,
        working_log_base_sha,
        commit_sha,
        Some(&pathspecs),
    )?;

    // Get git diff stats for the commit
    let (git_diff_added_lines, git_diff_deleted_lines) =
        get_git_diff_stats(repository, commit_sha)?;

    // Generate CommitStats from the authorship log
    Ok(stats_from_authorship_log(
        Some(&authorship_log),
        git_diff_added_lines,
        git_diff_deleted_lines,
    ))
}

mod tests {
    use crate::git::find_repository_in_path;

    use super::*;

    #[test]
    fn test_simulate_post_commit() {
        let repository = find_repository_in_path(".").unwrap();
        let working_log_base_sha = "a99bf9310936936f0c0e49a3ef37be80a5e3dc51";
        let commit_sha = "1234567890";
        let stats = simulate_post_commit(&repository, working_log_base_sha, commit_sha).unwrap();
    }
}
