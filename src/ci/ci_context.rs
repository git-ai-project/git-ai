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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_utils::TmpRepo;
    use std::fs;

    #[test]
    fn test_ci_event_debug() {
        let event = CiEvent::Merge {
            merge_commit_sha: "abc123".to_string(),
            head_ref: "feature".to_string(),
            head_sha: "def456".to_string(),
            base_ref: "main".to_string(),
            base_sha: "ghi789".to_string(),
        };

        let debug_str = format!("{:?}", event);
        assert!(debug_str.contains("Merge"));
        assert!(debug_str.contains("abc123"));
        assert!(debug_str.contains("feature"));
    }

    #[test]
    fn test_ci_run_result_debug() {
        let result = CiRunResult::SkippedSimpleMerge;
        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("SkippedSimpleMerge"));

        let result2 = CiRunResult::SkippedFastForward;
        let debug_str2 = format!("{:?}", result2);
        assert!(debug_str2.contains("SkippedFastForward"));

        let result3 = CiRunResult::NoAuthorshipAvailable;
        let debug_str3 = format!("{:?}", result3);
        assert!(debug_str3.contains("NoAuthorshipAvailable"));
    }

    #[test]
    fn test_ci_context_with_repository() {
        let test_repo = TmpRepo::new().unwrap();
        let repo_path = test_repo.path().to_path_buf();
        let repo =
            crate::git::repository::find_repository_in_path(repo_path.to_str().unwrap()).unwrap();

        let event = CiEvent::Merge {
            merge_commit_sha: "abc".to_string(),
            head_ref: "feature".to_string(),
            head_sha: "def".to_string(),
            base_ref: "main".to_string(),
            base_sha: "ghi".to_string(),
        };

        let context = CiContext::with_repository(repo, event);
        assert!(context.temp_dir.as_os_str().is_empty());
    }

    #[test]
    fn test_ci_context_teardown_empty_temp_dir() {
        let test_repo = TmpRepo::new().unwrap();
        let repo_path = test_repo.path().to_path_buf();
        let repo =
            crate::git::repository::find_repository_in_path(repo_path.to_str().unwrap()).unwrap();

        let event = CiEvent::Merge {
            merge_commit_sha: "abc".to_string(),
            head_ref: "feature".to_string(),
            head_sha: "def".to_string(),
            base_ref: "main".to_string(),
            base_sha: "ghi".to_string(),
        };

        let context = CiContext::with_repository(repo, event);
        let result = context.teardown();
        assert!(result.is_ok());
    }

    #[test]
    fn test_ci_context_teardown_with_temp_dir() {
        let test_repo = TmpRepo::new().unwrap();
        let repo_path = test_repo.path().to_path_buf();
        let repo =
            crate::git::repository::find_repository_in_path(repo_path.to_str().unwrap()).unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        let temp_path = temp_dir.path().to_path_buf();

        // Write a test file
        fs::write(temp_path.join("test.txt"), "test").unwrap();

        let event = CiEvent::Merge {
            merge_commit_sha: "abc".to_string(),
            head_ref: "feature".to_string(),
            head_sha: "def".to_string(),
            base_ref: "main".to_string(),
            base_sha: "ghi".to_string(),
        };

        let context = CiContext {
            repo,
            event,
            temp_dir: temp_path.clone(),
        };

        // Directory should exist before teardown
        assert!(temp_path.exists());

        let result = context.teardown();
        assert!(result.is_ok());

        // Directory should be removed after teardown
        assert!(!temp_path.exists());
    }

    #[test]
    fn test_get_rebased_commits_linear_history() {
        let test_repo = TmpRepo::new().unwrap();
        let repo = test_repo.gitai_repo();

        // Create a linear commit history
        let file_path = test_repo.path().join("test.txt");

        // First commit
        fs::write(&file_path, "commit 1").unwrap();
        let mut index = test_repo.repo().index().unwrap();
        index.add_path(std::path::Path::new("test.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = test_repo.repo().find_tree(tree_id).unwrap();
        let sig = test_repo.repo().signature().unwrap();
        let commit1 = test_repo
            .repo()
            .commit(Some("HEAD"), &sig, &sig, "Commit 1", &tree, &[])
            .unwrap();

        // Second commit
        fs::write(&file_path, "commit 2").unwrap();
        index.add_path(std::path::Path::new("test.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = test_repo.repo().find_tree(tree_id).unwrap();
        let parent1 = test_repo.repo().find_commit(commit1).unwrap();
        let commit2 = test_repo
            .repo()
            .commit(Some("HEAD"), &sig, &sig, "Commit 2", &tree, &[&parent1])
            .unwrap();

        // Third commit
        fs::write(&file_path, "commit 3").unwrap();
        index.add_path(std::path::Path::new("test.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = test_repo.repo().find_tree(tree_id).unwrap();
        let parent2 = test_repo.repo().find_commit(commit2).unwrap();
        let commit3 = test_repo
            .repo()
            .commit(Some("HEAD"), &sig, &sig, "Commit 3", &tree, &[&parent2])
            .unwrap();

        let repo_path = test_repo.path().to_path_buf();
        let gitai_repo =
            crate::git::repository::find_repository_in_path(repo_path.to_str().unwrap()).unwrap();

        let event = CiEvent::Merge {
            merge_commit_sha: commit3.to_string(),
            head_ref: "HEAD".to_string(),
            head_sha: commit3.to_string(),
            base_ref: "main".to_string(),
            base_sha: commit1.to_string(),
        };

        let context = CiContext::with_repository(gitai_repo, event);

        // Get the last 3 commits
        let commits = context.get_rebased_commits(&commit3.to_string(), 3);
        assert_eq!(commits.len(), 3);
        assert_eq!(commits[2], commit3.to_string());
        assert_eq!(commits[1], commit2.to_string());
        assert_eq!(commits[0], commit1.to_string());
    }

    #[test]
    fn test_get_rebased_commits_more_than_available() {
        let test_repo = TmpRepo::new().unwrap();
        let repo = test_repo.gitai_repo();

        // Create single commit
        let file_path = test_repo.path().join("test.txt");
        fs::write(&file_path, "content").unwrap();
        let mut index = test_repo.repo().index().unwrap();
        index.add_path(std::path::Path::new("test.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = test_repo.repo().find_tree(tree_id).unwrap();
        let sig = test_repo.repo().signature().unwrap();
        let commit = test_repo
            .repo()
            .commit(Some("HEAD"), &sig, &sig, "Commit", &tree, &[])
            .unwrap();

        let repo_path = test_repo.path().to_path_buf();
        let gitai_repo =
            crate::git::repository::find_repository_in_path(repo_path.to_str().unwrap()).unwrap();

        let event = CiEvent::Merge {
            merge_commit_sha: commit.to_string(),
            head_ref: "HEAD".to_string(),
            head_sha: commit.to_string(),
            base_ref: "main".to_string(),
            base_sha: "base".to_string(),
        };

        let context = CiContext::with_repository(gitai_repo, event);

        // Try to get 10 commits when only 1 exists
        let commits = context.get_rebased_commits(&commit.to_string(), 10);
        // Should stop at the root commit
        assert_eq!(commits.len(), 1);
    }

    #[test]
    fn test_ci_context_debug() {
        let test_repo = TmpRepo::new().unwrap();
        let repo_path = test_repo.path().to_path_buf();
        let repo =
            crate::git::repository::find_repository_in_path(repo_path.to_str().unwrap()).unwrap();

        let event = CiEvent::Merge {
            merge_commit_sha: "abc".to_string(),
            head_ref: "feature".to_string(),
            head_sha: "def".to_string(),
            base_ref: "main".to_string(),
            base_sha: "ghi".to_string(),
        };

        let context = CiContext::with_repository(repo, event);
        let debug_str = format!("{:?}", context);
        assert!(debug_str.contains("CiContext"));
    }
}
