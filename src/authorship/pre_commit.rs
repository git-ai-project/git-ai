use crate::authorship::working_log::CheckpointKind;
use crate::error::GitAiError;
use crate::git::repository::Repository;

pub fn pre_commit(repo: &Repository, default_author: String) -> Result<(), GitAiError> {
    // Run checkpoint as human editor.
    let result: Result<(usize, usize, usize), GitAiError> = crate::commands::checkpoint::run(
        repo,
        &default_author,
        CheckpointKind::Human,
        false,
        false,
        true,
        None,
        true, // should skip if NO AI CHECKPOINTS
    );
    result.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_utils::TmpRepo;
    use std::fs;

    #[test]
    fn test_pre_commit_empty_repo() {
        let test_repo = TmpRepo::new().unwrap();
        let repo = test_repo.gitai_repo();

        // Should handle empty repo gracefully
        let result = pre_commit(repo, "test_author".to_string());
        // May succeed or fail depending on repo state, but shouldn't panic
        let _ = result;
    }

    #[test]
    fn test_pre_commit_with_staged_changes() {
        let test_repo = TmpRepo::new().unwrap();
        let repo = test_repo.gitai_repo();

        // Create and stage a file
        let file_path = test_repo.path().join("test.txt");
        fs::write(&file_path, "test content").unwrap();

        let mut index = test_repo.repo().index().unwrap();
        index.add_path(std::path::Path::new("test.txt")).unwrap();
        index.write().unwrap();

        let result = pre_commit(repo, "test_author".to_string());
        // Should not panic
        let _ = result;
    }

    #[test]
    fn test_pre_commit_no_changes() {
        let test_repo = TmpRepo::new().unwrap();
        let repo = test_repo.gitai_repo();

        // Create initial commit
        let file_path = test_repo.path().join("initial.txt");
        fs::write(&file_path, "initial").unwrap();

        let mut index = test_repo.repo().index().unwrap();
        index.add_path(std::path::Path::new("initial.txt")).unwrap();
        index.write().unwrap();

        let tree_id = index.write_tree().unwrap();
        let tree = test_repo.repo().find_tree(tree_id).unwrap();
        let sig = test_repo.repo().signature().unwrap();

        test_repo
            .repo()
            .commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
            .unwrap();

        // Run pre_commit with no staged changes
        let result = pre_commit(repo, "test_author".to_string());
        // Should handle gracefully
        let _ = result;
    }

    #[test]
    fn test_pre_commit_result_mapping() {
        let test_repo = TmpRepo::new().unwrap();
        let repo = test_repo.gitai_repo();

        let result = pre_commit(repo, "author".to_string());

        // Result should be either Ok(()) or Err(GitAiError)
        match result {
            Ok(()) => {
                // Success case
            }
            Err(_) => {
                // Error case is also acceptable
            }
        }
    }
}
