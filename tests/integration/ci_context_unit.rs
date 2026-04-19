use crate::repos::test_repo::TestRepo;
use git_ai::ci::ci_context::{CiContext, CiEvent, CiRunResult};
use git_ai::git::repository::find_repository_in_path;
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
    let repo = TestRepo::new();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let event = CiEvent::Merge {
        merge_commit_sha: "abc".to_string(),
        head_ref: "feature".to_string(),
        head_sha: "def".to_string(),
        base_ref: "main".to_string(),
        base_sha: "ghi".to_string(),
    };

    let context = CiContext::with_repository(gitai_repo, event);
    assert!(context.temp_dir.as_os_str().is_empty());
}

#[test]
fn test_ci_context_teardown_empty_temp_dir() {
    let repo = TestRepo::new();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let event = CiEvent::Merge {
        merge_commit_sha: "abc".to_string(),
        head_ref: "feature".to_string(),
        head_sha: "def".to_string(),
        base_ref: "main".to_string(),
        base_sha: "ghi".to_string(),
    };

    let context = CiContext::with_repository(gitai_repo, event);
    let result = context.teardown();
    assert!(result.is_ok());
}

#[test]
fn test_ci_context_teardown_with_temp_dir() {
    let repo = TestRepo::new();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
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
        repo: gitai_repo,
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
    let repo = TestRepo::new();

    // First commit
    fs::write(repo.path().join("test.txt"), "commit 1").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "Commit 1"]).unwrap();
    let commit1 = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Second commit
    fs::write(repo.path().join("test.txt"), "commit 2").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "Commit 2"]).unwrap();
    let commit2 = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Third commit
    fs::write(repo.path().join("test.txt"), "commit 3").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "Commit 3"]).unwrap();
    let commit3 = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let event = CiEvent::Merge {
        merge_commit_sha: commit3.clone(),
        head_ref: "HEAD".to_string(),
        head_sha: commit3.clone(),
        base_ref: "main".to_string(),
        base_sha: commit1.clone(),
    };
    let context = CiContext::with_repository(gitai_repo, event);

    let commits = context.get_rebased_commits(&commit3, 3);
    assert_eq!(commits.len(), 3);
    assert_eq!(commits[2], commit3);
    assert_eq!(commits[1], commit2);
    assert_eq!(commits[0], commit1);
}

#[test]
fn test_get_rebased_commits_more_than_available() {
    let repo = TestRepo::new();

    // Create single commit
    fs::write(repo.path().join("test.txt"), "content").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "Commit"]).unwrap();
    let commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let event = CiEvent::Merge {
        merge_commit_sha: commit.clone(),
        head_ref: "HEAD".to_string(),
        head_sha: commit.clone(),
        base_ref: "main".to_string(),
        base_sha: "base".to_string(),
    };
    let context = CiContext::with_repository(gitai_repo, event);

    // Try to get 10 commits when only 1 exists
    let commits = context.get_rebased_commits(&commit, 10);
    // Should stop at the root commit
    assert_eq!(commits.len(), 1);
}

#[test]
fn test_ci_context_debug() {
    let repo = TestRepo::new();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let event = CiEvent::Merge {
        merge_commit_sha: "abc".to_string(),
        head_ref: "feature".to_string(),
        head_sha: "def".to_string(),
        base_ref: "main".to_string(),
        base_sha: "ghi".to_string(),
    };

    let context = CiContext::with_repository(gitai_repo, event);
    let debug_str = format!("{:?}", context);
    assert!(debug_str.contains("CiContext"));
}
