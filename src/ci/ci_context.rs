use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::rebase_authorship::{
    rewrite_authorship_after_rebase_v2, rewrite_authorship_after_squash_or_rebase,
};
use crate::error::GitAiError;
use crate::git::refs::{get_reference_as_authorship_log_v3, show_authorship_note};
use crate::git::repository::{CommitRange, Repository};
use crate::git::sync_authorship::fetch_authorship_notes;
use std::fs;
use std::path::PathBuf;

#[derive(Debug)]
pub enum CiEvent {
    Merge {
        merge_commit_sha: String,
        head_ref: String,
        head_sha: String,
        base_ref: String,
        #[allow(dead_code)]
        base_sha: String,
    },
}

/// Result of running CiContext
#[derive(Debug)]
pub enum CiRunResult {
    /// Authorship was successfully rewritten for a squash/rebase merge
    AuthorshipRewritten {
        #[allow(dead_code)]
        authorship_log: AuthorshipLog,
    },
    /// Skipped: merge commit has multiple parents (simple merge - authorship already present)
    SkippedSimpleMerge,
    /// Skipped: merge commit equals head (fast-forward - no rewrite needed)
    SkippedFastForward,
    /// Authorship already exists for this commit
    AlreadyExists {
        #[allow(dead_code)]
        authorship_log: AuthorshipLog,
    },
    /// No AI authorship to track (pre-git-ai commits or human-only code)
    NoAuthorshipAvailable,
}

#[derive(Debug)]
pub struct CiContext {
    pub repo: Repository,
    pub event: CiEvent,
    pub temp_dir: PathBuf,
}

impl CiContext {
    /// Create a CiContext with an existing repository (no automatic cleanup)
    #[allow(dead_code)]
    pub fn with_repository(repo: Repository, event: CiEvent) -> Self {
        CiContext {
            repo,
            event,
            temp_dir: PathBuf::new(), // Empty path indicates no cleanup needed
        }
    }

    pub fn run(&self) -> Result<CiRunResult, GitAiError> {
        match &self.event {
            CiEvent::Merge {
                merge_commit_sha,
                head_ref,
                head_sha,
                base_ref,
                base_sha: _,
            } => {
                println!("Working repository is in {}", self.repo.path().display());

                println!("Fetching authorship history");
                // Ensure we have the full authorship history before checking for existing notes
                fetch_authorship_notes(&self.repo, "origin")?;
                println!("Fetched authorship history");

                // Check if authorship already exists for this commit
                match get_reference_as_authorship_log_v3(&self.repo, merge_commit_sha) {
                    Ok(existing_log) => {
                        println!("{} already has authorship", merge_commit_sha);
                        return Ok(CiRunResult::AlreadyExists {
                            authorship_log: existing_log,
                        });
                    }
                    Err(e) => {
                        if show_authorship_note(&self.repo, merge_commit_sha).is_some() {
                            return Err(e);
                        }
                    }
                }

                // Only handle squash or rebase-like merges.
                // Skip simple merge commits (2+ parents) and fast-forward merges (merge commit == head).
                let merge_commit = self.repo.find_commit(merge_commit_sha.clone())?;
                let parent_count = merge_commit.parents().count();
                if parent_count > 1 {
                    println!(
                        "{} has {} parents (simple merge)",
                        merge_commit_sha, parent_count
                    );
                    return Ok(CiRunResult::SkippedSimpleMerge);
                }

                if merge_commit_sha == head_sha {
                    println!(
                        "{} equals head {} (fast-forward)",
                        merge_commit_sha, head_sha
                    );
                    return Ok(CiRunResult::SkippedFastForward);
                }
                println!(
                    "Rewriting authorship for {} -> {} (squash or rebase-like merge)",
                    head_sha, merge_commit_sha
                );
                println!("Fetching base branch {}", base_ref);
                // Ensure we have all the required commits from the base branch
                self.repo.fetch_branch(base_ref, "origin").map_err(|e| {
                    GitAiError::Generic(format!(
                        "Failed to fetch base branch '{}': {}",
                        base_ref, e
                    ))
                })?;
                println!("Fetched base branch.");

                // Detect squash vs rebase merge by counting commits
                // For squash: N original commits → 1 merge commit
                // For rebase: N original commits → N rebased commits
                let merge_base = self
                    .repo
                    .merge_base(head_sha.to_string(), base_ref.to_string())
                    .ok();

                let original_commits = if let Some(ref base) = merge_base {
                    CommitRange::new_infer_refname(
                        &self.repo,
                        base.clone(),
                        head_sha.to_string(),
                        None,
                    )
                    .map(|r| r.all_commits())
                    .unwrap_or_else(|_| vec![head_sha.to_string()])
                } else {
                    vec![head_sha.to_string()]
                };

                println!(
                    "Original commits in PR: {} (from merge base {:?})",
                    original_commits.len(),
                    merge_base
                );

                // For multi-commit PRs, check if this is a rebase merge (multiple new commits)
                // by walking back from merge_commit_sha
                if original_commits.len() > 1 {
                    // Try to find the new rebased commits
                    // Walk back from merge_commit_sha the same number of commits as original
                    let new_commits =
                        self.get_rebased_commits(merge_commit_sha, original_commits.len());

                    if new_commits.len() == original_commits.len() {
                        println!(
                            "Detected rebase merge: {} original -> {} new commits",
                            original_commits.len(),
                            new_commits.len()
                        );
                        // Rebase merge - use v2 which writes authorship to each rebased commit
                        rewrite_authorship_after_rebase_v2(
                            &self.repo,
                            head_sha,
                            &original_commits,
                            &new_commits,
                            "", // human_author not used
                        )?;
                    } else {
                        println!(
                            "Detected squash merge: {} original commits -> 1 merge commit",
                            original_commits.len()
                        );
                        // Squash merge - use existing function which writes to single merge commit
                        rewrite_authorship_after_squash_or_rebase(
                            &self.repo,
                            head_ref,
                            base_ref,
                            head_sha,
                            merge_commit_sha,
                            false,
                        )?;
                    }
                } else {
                    // Single commit - use squash_or_rebase (handles both cases)
                    println!("Single commit PR, using squash/rebase handler");
                    rewrite_authorship_after_squash_or_rebase(
                        &self.repo,
                        head_ref,
                        base_ref,
                        head_sha,
                        merge_commit_sha,
                        false,
                    )?;
                }
                println!("Rewrote authorship.");

                // Check if authorship was created for THIS specific commit
                match get_reference_as_authorship_log_v3(&self.repo, merge_commit_sha) {
                    Ok(authorship_log) => {
                        println!("Pushing authorship...");
                        self.repo.push_authorship("origin")?;
                        println!("Pushed authorship. Done.");
                        Ok(CiRunResult::AuthorshipRewritten { authorship_log })
                    }
                    Err(e) => {
                        if show_authorship_note(&self.repo, merge_commit_sha).is_some() {
                            return Err(e);
                        }
                        println!(
                            "No AI authorship to track for this commit (no AI-touched files in PR)"
                        );
                        Ok(CiRunResult::NoAuthorshipAvailable)
                    }
                }
            }
        }
    }

    pub fn teardown(&self) -> Result<(), GitAiError> {
        // Skip cleanup if temp_dir is empty (repository was provided externally)
        if self.temp_dir.as_os_str().is_empty() {
            return Ok(());
        }
        fs::remove_dir_all(self.temp_dir.clone())?;
        Ok(())
    }

    /// Get the rebased commits by walking back from merge_commit_sha.
    /// For a rebase merge with N original commits, there should be N new commits
    /// ending at merge_commit_sha.
    fn get_rebased_commits(&self, merge_commit_sha: &str, expected_count: usize) -> Vec<String> {
        let mut commits = Vec::new();
        let mut current_sha = merge_commit_sha.to_string();

        for _ in 0..expected_count {
            commits.push(current_sha.clone());

            // Get the parent of current commit
            match self.repo.find_commit(current_sha.clone()) {
                Ok(commit) => {
                    let parents: Vec<_> = commit.parents().collect();
                    if parents.len() != 1 {
                        // Not a linear chain (merge commit or root), stop here
                        break;
                    }
                    current_sha = parents[0].id().to_string();
                }
                Err(_) => break,
            }
        }

        // Reverse to get oldest-to-newest order (same as original_commits)
        commits.reverse();
        commits
    }
}
