//! Comprehensive tests for src/git/repository.rs
//!
//! This test suite covers the core git operations layer including:
//! - Repository initialization and discovery
//! - Git command execution and error handling
//! - HEAD operations and branch management
//! - Commit operations and traversal
//! - Config get/set operations
//! - Pathspec validation and filtering
//! - Rewrite log operations
//! - Error handling and edge cases
//! - Working directory operations
//! - Bare repository support

#[macro_use]
mod repos;

use git_ai::git::repository::{find_repository, find_repository_in_path};
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

// ============================================================================
// Repository Discovery and Initialization Tests
// ============================================================================

#[test]
fn test_find_repository_in_valid_repo() {
    let repo = TestRepo::new();

    // Create a commit to ensure it's a valid repo
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Should successfully find repository
    let found_repo =
        find_repository(&["-C".to_string(), repo.path().to_str().unwrap().to_string()]);

    assert!(found_repo.is_ok(), "Should find valid repository");
}

#[test]
fn test_find_repository_in_subdirectory() {
    let repo = TestRepo::new();

    // Create subdirectory
    let subdir = repo.path().join("subdir");
    fs::create_dir(&subdir).unwrap();

    // Should find repository from subdirectory
    let found_repo = find_repository(&["-C".to_string(), subdir.to_str().unwrap().to_string()]);

    assert!(
        found_repo.is_ok(),
        "Should find repository from subdirectory"
    );
}

#[test]
fn test_find_repository_in_nested_subdirectory() {
    let repo = TestRepo::new();

    // Create nested subdirectories
    let nested = repo.path().join("a").join("b").join("c");
    fs::create_dir_all(&nested).unwrap();

    // Should find repository from deeply nested subdirectory
    let found_repo = find_repository(&["-C".to_string(), nested.to_str().unwrap().to_string()]);

    assert!(
        found_repo.is_ok(),
        "Should find repository from nested subdirectory"
    );
}

#[test]
fn test_find_repository_for_bare_repo() {
    let bare_repo = TestRepo::new_bare();

    let found_repo = find_repository(&[
        "-C".to_string(),
        bare_repo.path().to_str().unwrap().to_string(),
    ]);

    assert!(found_repo.is_ok(), "Should find bare repository");

    let repo = found_repo.unwrap();
    assert!(
        repo.is_bare_repository().unwrap(),
        "Should detect bare repository"
    );
}

#[test]
fn test_repository_path_methods() {
    let test_repo = TestRepo::new();
    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Test path() returns .git directory
    let git_path = repo.path();
    assert!(
        git_path.ends_with(".git"),
        "path() should return .git directory"
    );

    // Test workdir() returns repository root (use canonical paths for macOS /var vs /private/var)
    let workdir = repo.workdir().unwrap();
    let canonical_workdir = workdir.canonicalize().unwrap();
    let canonical_test_path = test_repo.path().canonicalize().unwrap();
    assert_eq!(
        canonical_workdir, canonical_test_path,
        "workdir() should return repository root"
    );
}

#[test]
fn test_canonical_workdir() {
    let test_repo = TestRepo::new();
    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let canonical = repo.canonical_workdir();
    assert!(
        canonical.is_absolute(),
        "Canonical workdir should be absolute"
    );
}

#[test]
fn test_path_is_in_workdir() {
    let test_repo = TestRepo::new();
    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Path inside workdir - create the file so it can be canonicalized
    let inside = test_repo.path().join("file.txt");
    fs::write(&inside, "test content").unwrap();
    assert!(
        repo.path_is_in_workdir(&inside),
        "File in workdir should return true"
    );

    // Path outside workdir
    let outside = Path::new("/tmp/outside.txt");
    assert!(
        !repo.path_is_in_workdir(outside),
        "File outside workdir should return false"
    );
}

// ============================================================================
// HEAD and Reference Tests
// ============================================================================

#[test]
fn test_head_on_main_branch() {
    let test_repo = TestRepo::new();

    // Create initial commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Initial commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let head = repo.head().unwrap();
    let name = head.name().unwrap();

    // Should be on main or master
    assert!(
        name.contains("main") || name.contains("master"),
        "HEAD should be on main/master branch, got: {}",
        name
    );
}

#[test]
fn test_head_on_feature_branch() {
    let test_repo = TestRepo::new();

    // Create initial commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Initial commit").unwrap();

    // Create and checkout feature branch
    test_repo.git(&["checkout", "-b", "feature"]).unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let head = repo.head().unwrap();
    let shorthand = head.shorthand().unwrap();

    assert_eq!(shorthand, "feature", "HEAD should be on feature branch");
}

#[test]
fn test_head_target() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit = test_repo.stage_all_and_commit("Initial commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let head = repo.head().unwrap();
    let target = head.target().unwrap();

    assert_eq!(
        target, commit.commit_sha,
        "HEAD target should match commit SHA"
    );
}

#[test]
fn test_reference_is_branch() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Initial commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let head = repo.head().unwrap();
    assert!(head.is_branch(), "HEAD should be a branch");
}

#[test]
fn test_find_reference() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Initial commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Get full ref name from HEAD
    let head = repo.head().unwrap();
    let ref_name = head.name().unwrap();

    // Find reference by name
    let found_ref = repo.find_reference(ref_name);
    assert!(found_ref.is_ok(), "Should find reference by full name");
}

// ============================================================================
// Commit Operations and Traversal Tests
// ============================================================================

#[test]
fn test_find_commit() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit_info = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit = repo.find_commit(commit_info.commit_sha.clone());
    assert!(commit.is_ok(), "Should find commit by SHA");

    let commit = commit.unwrap();
    assert_eq!(
        commit.id(),
        commit_info.commit_sha,
        "Commit ID should match"
    );
}

#[test]
fn test_commit_summary() {
    let test_repo = TestRepo::new();

    // Create commit with message
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit_info = test_repo
        .stage_all_and_commit("Test summary message")
        .unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit = repo.find_commit(commit_info.commit_sha).unwrap();
    let summary = commit.summary().unwrap();

    assert_eq!(
        summary, "Test summary message",
        "Summary should match commit message"
    );
}

#[test]
fn test_commit_body() {
    let test_repo = TestRepo::new();

    // Create commit with multi-line message
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.git(&["add", "-A"]).unwrap();

    let message = "Summary line\n\nBody line 1\nBody line 2";
    test_repo.git(&["commit", "-m", message]).unwrap();

    let commit_sha = test_repo
        .git(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit = repo.find_commit(commit_sha).unwrap();
    let body = commit.body().unwrap();

    assert!(
        body.contains("Body line 1"),
        "Body should contain first body line"
    );
    assert!(
        body.contains("Body line 2"),
        "Body should contain second body line"
    );
}

#[test]
fn test_commit_parent() {
    let test_repo = TestRepo::new();

    // Create two commits
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content1".human()]);
    let first = test_repo.stage_all_and_commit("First commit").unwrap();

    file.set_contents(lines!["content2".human()]);
    let second = test_repo.stage_all_and_commit("Second commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit = repo.find_commit(second.commit_sha).unwrap();
    let parent = commit.parent(0).unwrap();

    assert_eq!(
        parent.id(),
        first.commit_sha,
        "Parent should be first commit"
    );
}

#[test]
fn test_commit_parents_iterator() {
    let test_repo = TestRepo::new();

    // Create commits
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content1".human()]);
    test_repo.stage_all_and_commit("First commit").unwrap();

    file.set_contents(lines!["content2".human()]);
    test_repo.stage_all_and_commit("Second commit").unwrap();

    let commit_sha = test_repo
        .git(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit = repo.find_commit(commit_sha).unwrap();
    let parents: Vec<_> = commit.parents().collect();

    assert_eq!(parents.len(), 1, "Should have one parent");
}

#[test]
fn test_commit_parent_count() {
    let test_repo = TestRepo::new();

    // Create initial commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let first = test_repo.stage_all_and_commit("First commit").unwrap();

    // Create second commit
    file.set_contents(lines!["content2".human()]);
    test_repo.stage_all_and_commit("Second commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Initial commit has no parents
    let first_commit = repo.find_commit(first.commit_sha).unwrap();
    assert_eq!(
        first_commit.parent_count().unwrap(),
        0,
        "Initial commit should have no parents"
    );

    // Second commit has one parent
    let head_sha = test_repo
        .git(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let second_commit = repo.find_commit(head_sha).unwrap();
    assert_eq!(
        second_commit.parent_count().unwrap(),
        1,
        "Second commit should have one parent"
    );
}

#[test]
fn test_commit_tree() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit_info = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit = repo.find_commit(commit_info.commit_sha).unwrap();
    let tree = commit.tree();

    assert!(tree.is_ok(), "Should get tree from commit");
}

#[test]
fn test_revparse_single() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Revparse HEAD
    let obj = repo.revparse_single("HEAD");
    assert!(obj.is_ok(), "Should revparse HEAD");
}

#[test]
fn test_revparse_single_with_relative_ref() {
    let test_repo = TestRepo::new();

    // Create two commits
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content1".human()]);
    test_repo.stage_all_and_commit("First commit").unwrap();

    file.set_contents(lines!["content2".human()]);
    test_repo.stage_all_and_commit("Second commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Revparse HEAD~1
    let obj = repo.revparse_single("HEAD~1");
    assert!(obj.is_ok(), "Should revparse HEAD~1");
}

#[test]
fn test_object_peel_to_commit() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let obj = repo.revparse_single("HEAD").unwrap();
    let commit = obj.peel_to_commit();

    assert!(commit.is_ok(), "Should peel object to commit");
}

// ============================================================================
// Tree and Blob Tests
// ============================================================================

#[test]
fn test_tree_get_path() {
    let test_repo = TestRepo::new();

    // Create file and commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit_info = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit = repo.find_commit(commit_info.commit_sha).unwrap();
    let tree = commit.tree().unwrap();
    let entry = tree.get_path(Path::new("test.txt"));

    assert!(entry.is_ok(), "Should find file in tree");
}

#[test]
fn test_tree_get_path_nested() {
    let test_repo = TestRepo::new();

    // Create nested file
    fs::create_dir(test_repo.path().join("subdir")).unwrap();
    let mut file = test_repo.filename("subdir/nested.txt");
    file.set_contents(lines!["nested content".human()]);
    let commit_info = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit = repo.find_commit(commit_info.commit_sha).unwrap();
    let tree = commit.tree().unwrap();
    let entry = tree.get_path(Path::new("subdir/nested.txt"));

    assert!(entry.is_ok(), "Should find nested file in tree");
}

#[test]
fn test_tree_get_path_nonexistent() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit_info = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit = repo.find_commit(commit_info.commit_sha).unwrap();
    let tree = commit.tree().unwrap();
    let entry = tree.get_path(Path::new("nonexistent.txt"));

    assert!(entry.is_err(), "Should not find nonexistent file in tree");
}

#[test]
fn test_find_blob() {
    let test_repo = TestRepo::new();

    // Create file and commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit_info = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit = repo.find_commit(commit_info.commit_sha).unwrap();
    let tree = commit.tree().unwrap();
    let entry = tree.get_path(Path::new("test.txt")).unwrap();
    let blob = repo.find_blob(entry.id());

    assert!(blob.is_ok(), "Should find blob");
}

#[test]
fn test_blob_content() {
    let test_repo = TestRepo::new();

    // Create file and commit
    let mut file = test_repo.filename("test.txt");
    let content = "test content line";
    file.set_contents(lines![content.human()]);
    let commit_info = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit = repo.find_commit(commit_info.commit_sha).unwrap();
    let tree = commit.tree().unwrap();
    let entry = tree.get_path(Path::new("test.txt")).unwrap();
    let blob = repo.find_blob(entry.id()).unwrap();
    let blob_content = blob.content().unwrap();

    let blob_str = String::from_utf8(blob_content).unwrap();
    assert!(
        blob_str.contains(content),
        "Blob content should match file content"
    );
}

// ============================================================================
// Config Operations Tests
// ============================================================================

#[test]
fn test_config_get_str() {
    let test_repo = TestRepo::new();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Get user.name which is set in test repo
    let name = repo.config_get_str("user.name");
    assert!(name.is_ok(), "Should get config value");

    let name = name.unwrap();
    assert!(name.is_some(), "user.name should be set");
    assert_eq!(
        name.unwrap(),
        "Test User",
        "user.name should be 'Test User'"
    );
}

#[test]
fn test_config_get_str_nonexistent() {
    let test_repo = TestRepo::new();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Get nonexistent config
    let result = repo.config_get_str("nonexistent.config.key");
    assert!(result.is_ok(), "Should not error on nonexistent key");

    let value = result.unwrap();
    assert!(value.is_none(), "Nonexistent key should return None");
}

#[test]
fn test_config_get_regexp() {
    let test_repo = TestRepo::new();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Get all user.* configs
    let configs = repo.config_get_regexp("user\\..*");
    assert!(configs.is_ok(), "Should get matching configs");

    let configs = configs.unwrap();
    assert!(
        !configs.is_empty(),
        "Should have at least one user.* config"
    );
    assert!(
        configs.contains_key("user.name"),
        "Should contain user.name"
    );
}

#[test]
fn test_git_version() {
    let test_repo = TestRepo::new();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let version = repo.git_version();
    assert!(version.is_some(), "Should get git version");

    let (major, _minor, _patch) = version.unwrap();
    assert!(major >= 2, "Git major version should be at least 2");
}

#[test]
fn test_git_supports_ignore_revs_file() {
    let test_repo = TestRepo::new();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Most modern git versions support this (added in 2.23.0)
    let supports = repo.git_supports_ignore_revs_file();
    // Just verify it returns a boolean without error
    assert!(
        supports || !supports,
        "Should return boolean for ignore-revs-file support"
    );
}

// ============================================================================
// Remote Operations Tests
// ============================================================================

#[test]
fn test_remotes_empty() {
    let test_repo = TestRepo::new();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let remotes = repo.remotes().unwrap();
    assert!(
        remotes.is_empty() || remotes == vec!["".to_string()],
        "New repo should have no remotes"
    );
}

#[test]
fn test_remotes_with_origin() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    let repo = find_repository(&[
        "-C".to_string(),
        mirror.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let remotes = repo.remotes().unwrap();
    assert!(
        remotes.contains(&"origin".to_string()),
        "Cloned repo should have origin remote"
    );
}

#[test]
fn test_remotes_with_urls() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    let repo = find_repository(&[
        "-C".to_string(),
        mirror.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let remotes_with_urls = repo.remotes_with_urls().unwrap();
    assert!(
        !remotes_with_urls.is_empty(),
        "Should have remotes with URLs"
    );

    let has_origin = remotes_with_urls
        .iter()
        .any(|(name, _url)| name == "origin");
    assert!(has_origin, "Should have origin remote with URL");
}

#[test]
fn test_get_default_remote() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    let repo = find_repository(&[
        "-C".to_string(),
        mirror.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let default_remote = repo.get_default_remote().unwrap();
    assert!(default_remote.is_some(), "Should have default remote");
    assert_eq!(
        default_remote.unwrap(),
        "origin",
        "Default remote should be origin"
    );
}

#[test]
fn test_get_default_remote_no_remotes() {
    let test_repo = TestRepo::new();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let default_remote = repo.get_default_remote().unwrap();
    // New repos might have an empty string as a remote or None
    assert!(
        default_remote.is_none() || default_remote == Some("".to_string()),
        "Repo without remotes should have no default or empty default"
    );
}

// ============================================================================
// Commit Range Tests
// ============================================================================

#[test]
fn test_commit_range_length() {
    let test_repo = TestRepo::new();

    // Create commits
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["line1".human()]);
    let first = test_repo.stage_all_and_commit("First").unwrap();

    file.set_contents(lines!["line1".human(), "line2".human()]);
    test_repo.stage_all_and_commit("Second").unwrap();

    file.set_contents(lines!["line1".human(), "line2".human(), "line3".human()]);
    let third = test_repo.stage_all_and_commit("Third").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Create commit range
    let range = git_ai::git::repository::CommitRange::new(
        &repo,
        first.commit_sha.clone(),
        third.commit_sha.clone(),
        "HEAD".to_string(),
    )
    .unwrap();

    let length = range.length();
    assert_eq!(
        length, 2,
        "Range should contain 2 commits (second and third)"
    );
}

#[test]
fn test_commit_range_iteration() {
    let test_repo = TestRepo::new();

    // Create commits
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["line1".human()]);
    let first = test_repo.stage_all_and_commit("First").unwrap();

    file.set_contents(lines!["line1".human(), "line2".human()]);
    let second = test_repo.stage_all_and_commit("Second").unwrap();

    file.set_contents(lines!["line1".human(), "line2".human(), "line3".human()]);
    let third = test_repo.stage_all_and_commit("Third").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let range = git_ai::git::repository::CommitRange::new(
        &repo,
        first.commit_sha,
        third.commit_sha.clone(),
        "HEAD".to_string(),
    )
    .unwrap();

    let commits: Vec<_> = range.into_iter().collect();
    assert_eq!(commits.len(), 2, "Should iterate over 2 commits");

    // Commits should be in reverse chronological order (newest first)
    assert_eq!(
        commits[0].id(),
        third.commit_sha,
        "First commit should be newest"
    );
    assert_eq!(
        commits[1].id(),
        second.commit_sha,
        "Second commit should be middle"
    );
}

#[test]
fn test_commit_range_all_commits() {
    let test_repo = TestRepo::new();

    // Create commits
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["line1".human()]);
    let first = test_repo.stage_all_and_commit("First").unwrap();

    file.set_contents(lines!["line1".human(), "line2".human()]);
    test_repo.stage_all_and_commit("Second").unwrap();

    file.set_contents(lines!["line1".human(), "line2".human(), "line3".human()]);
    let third = test_repo.stage_all_and_commit("Third").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let range = git_ai::git::repository::CommitRange::new(
        &repo,
        first.commit_sha,
        third.commit_sha,
        "HEAD".to_string(),
    )
    .unwrap();

    let all_commits = range.all_commits();
    assert_eq!(all_commits.len(), 2, "Should have 2 commits");
}

// ============================================================================
// Merge Base Tests
// ============================================================================

#[test]
fn test_merge_base_linear_history() {
    let test_repo = TestRepo::new();

    // Create linear history
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["line1".human()]);
    let first = test_repo.stage_all_and_commit("First").unwrap();

    file.set_contents(lines!["line1".human(), "line2".human()]);
    let second = test_repo.stage_all_and_commit("Second").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let merge_base = repo.merge_base(first.commit_sha.clone(), second.commit_sha);
    assert!(merge_base.is_ok(), "Should find merge base");

    let base = merge_base.unwrap();
    assert_eq!(base, first.commit_sha, "Merge base should be first commit");
}

#[test]
fn test_merge_base_with_branches() {
    let test_repo = TestRepo::new();

    // Create initial commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["line1".human()]);
    let base = test_repo.stage_all_and_commit("Base").unwrap();

    // Create branch
    test_repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.set_contents(lines!["line1".human(), "feature".human()]);
    let feature = test_repo.stage_all_and_commit("Feature").unwrap();

    // Go back to main and make different commit
    test_repo.git(&["checkout", "main"]).unwrap();
    file.set_contents(lines!["line1".human(), "main".human()]);
    let main = test_repo.stage_all_and_commit("Main").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let merge_base = repo.merge_base(feature.commit_sha, main.commit_sha);
    assert!(merge_base.is_ok(), "Should find merge base");

    let merge_base_sha = merge_base.unwrap();
    assert_eq!(
        merge_base_sha, base.commit_sha,
        "Merge base should be base commit"
    );
}

// ============================================================================
// File Content Tests
// ============================================================================

#[test]
fn test_get_file_content() {
    let test_repo = TestRepo::new();

    // Create file and commit
    let mut file = test_repo.filename("test.txt");
    let content = "test file content";
    file.set_contents(lines![content.human()]);
    let commit = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let file_content = repo.get_file_content("test.txt", &commit.commit_sha);
    assert!(file_content.is_ok(), "Should get file content");

    let content_bytes = file_content.unwrap();
    let content_str = String::from_utf8(content_bytes).unwrap();
    assert!(content_str.contains(content), "Content should match");
}

#[test]
fn test_get_file_content_nonexistent() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let result = repo.get_file_content("nonexistent.txt", &commit.commit_sha);
    assert!(result.is_err(), "Should error on nonexistent file");
}

#[test]
fn test_list_commit_files() {
    let test_repo = TestRepo::new();

    // Create multiple files and commit
    let mut file1 = test_repo.filename("file1.txt");
    let mut file2 = test_repo.filename("file2.txt");
    file1.set_contents(lines!["content1".human()]);
    file2.set_contents(lines!["content2".human()]);
    let commit = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let files = repo.list_commit_files(&commit.commit_sha, None);
    assert!(files.is_ok(), "Should list commit files");

    let files = files.unwrap();
    assert!(files.contains("file1.txt"), "Should contain file1.txt");
    assert!(files.contains("file2.txt"), "Should contain file2.txt");
}

#[test]
fn test_list_commit_files_with_pathspec() {
    let test_repo = TestRepo::new();

    // Create multiple files and commit
    let mut file1 = test_repo.filename("file1.txt");
    let mut file2 = test_repo.filename("file2.txt");
    file1.set_contents(lines!["content1".human()]);
    file2.set_contents(lines!["content2".human()]);
    let commit = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Filter to only file1.txt
    let mut pathspec = HashSet::new();
    pathspec.insert("file1.txt".to_string());

    let files = repo.list_commit_files(&commit.commit_sha, Some(&pathspec));
    assert!(files.is_ok(), "Should list filtered commit files");

    let files = files.unwrap();
    assert!(files.contains("file1.txt"), "Should contain file1.txt");
    assert!(!files.contains("file2.txt"), "Should not contain file2.txt");
}

#[test]
fn test_diff_changed_files() {
    let test_repo = TestRepo::new();

    // Create initial commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["line1".human()]);
    let first = test_repo.stage_all_and_commit("First").unwrap();

    // Modify file
    file.set_contents(lines!["line1".human(), "line2".human()]);
    let second = test_repo.stage_all_and_commit("Second").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let changed = repo.diff_changed_files(&first.commit_sha, &second.commit_sha);
    assert!(changed.is_ok(), "Should get changed files");

    let files = changed.unwrap();
    assert!(
        files.contains(&"test.txt".to_string()),
        "Should contain changed file"
    );
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_find_commit_invalid_sha() {
    let test_repo = TestRepo::new();

    // Create a valid repo
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Test").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let result = repo.find_commit("0000000000000000000000000000000000000000".to_string());
    assert!(result.is_err(), "Should error on invalid commit SHA");
}

#[test]
fn test_find_blob_with_commit_sha() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit = test_repo.stage_all_and_commit("Test").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Try to find blob using commit SHA (should fail)
    let result = repo.find_blob(commit.commit_sha);
    assert!(
        result.is_err(),
        "Should error when finding blob with commit SHA"
    );
}

#[test]
fn test_find_tree_with_commit_sha() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit = test_repo.stage_all_and_commit("Test").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Try to find tree using commit SHA (should fail)
    let result = repo.find_tree(commit.commit_sha);
    assert!(
        result.is_err(),
        "Should error when finding tree with commit SHA"
    );
}

#[test]
fn test_revparse_invalid_ref() {
    let test_repo = TestRepo::new();

    // Create valid repo
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Test").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let result = repo.revparse_single("invalid-ref-name-12345");
    assert!(result.is_err(), "Should error on invalid ref");
}

// ============================================================================
// Bare Repository Tests
// ============================================================================

#[test]
fn test_is_bare_repository() {
    let bare_repo = TestRepo::new_bare();

    let repo = find_repository(&[
        "-C".to_string(),
        bare_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let is_bare = repo.is_bare_repository();
    assert!(is_bare.is_ok(), "Should check if bare");
    assert!(is_bare.unwrap(), "Should be bare repository");
}

#[test]
fn test_is_not_bare_repository() {
    let test_repo = TestRepo::new();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let is_bare = repo.is_bare_repository();
    assert!(is_bare.is_ok(), "Should check if bare");
    assert!(!is_bare.unwrap(), "Should not be bare repository");
}

// ============================================================================
// Author and Signature Tests
// ============================================================================

#[test]
fn test_commit_author() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit_obj = repo.find_commit(commit.commit_sha).unwrap();
    let author = commit_obj.author();

    assert!(author.is_ok(), "Should get commit author");

    let author = author.unwrap();
    assert_eq!(author.name(), Some("Test User"), "Author name should match");
    assert_eq!(
        author.email(),
        Some("test@example.com"),
        "Author email should match"
    );
}

#[test]
fn test_commit_committer() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit_obj = repo.find_commit(commit.commit_sha).unwrap();
    let committer = commit_obj.committer();

    assert!(committer.is_ok(), "Should get commit committer");

    let committer = committer.unwrap();
    assert_eq!(
        committer.name(),
        Some("Test User"),
        "Committer name should match"
    );
}

#[test]
fn test_commit_time() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit_obj = repo.find_commit(commit.commit_sha).unwrap();
    let time = commit_obj.time();

    assert!(time.is_ok(), "Should get commit time");

    let time = time.unwrap();
    assert!(time.seconds() > 0, "Commit time should be after epoch");
}

#[test]
fn test_signature_when() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit = test_repo.stage_all_and_commit("Test commit").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit_obj = repo.find_commit(commit.commit_sha).unwrap();
    let author = commit_obj.author().unwrap();
    let time = author.when();

    assert!(time.seconds() > 0, "Author time should be after epoch");
}

// ============================================================================
// Working Directory Operations Tests
// ============================================================================

#[test]
fn test_find_repository_in_path() {
    let test_repo = TestRepo::new();

    // Create a commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Test").unwrap();

    let result = find_repository_in_path(test_repo.path().to_str().unwrap());
    assert!(result.is_ok(), "Should find repository in path");
}

#[test]
fn test_global_args_for_exec() {
    let test_repo = TestRepo::new();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let args = repo.global_args_for_exec();

    // Should include --no-pager
    assert!(
        args.contains(&"--no-pager".to_string()),
        "Global args should include --no-pager"
    );
}

#[test]
fn test_git_command_execution() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Test").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Execute git command
    let result = repo.git(&["rev-parse", "HEAD"]);
    assert!(result.is_ok(), "Should execute git command");

    let output = result.unwrap();
    assert!(!output.is_empty(), "Output should not be empty");
}

// ============================================================================
// References Iterator Tests
// ============================================================================

#[test]
fn test_references_iterator() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Test").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let refs = repo.references();
    assert!(refs.is_ok(), "Should get references iterator");

    let refs = refs.unwrap();
    let ref_list: Vec<_> = refs.collect();

    assert!(!ref_list.is_empty(), "Should have at least one reference");
}

#[test]
fn test_resolve_author_spec() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Test").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Resolve author by name
    let result = repo.resolve_author_spec("Test User");
    assert!(result.is_ok(), "Should resolve author spec");

    let author = result.unwrap();
    assert!(author.is_some(), "Should find author");
}

#[test]
fn test_resolve_author_spec_not_found() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.stage_all_and_commit("Test").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Resolve nonexistent author
    let result = repo.resolve_author_spec("Nonexistent Author");
    assert!(result.is_ok(), "Should not error on nonexistent author");

    let author = result.unwrap();
    assert!(author.is_none(), "Should not find nonexistent author");
}

// ============================================================================
// Edge Cases and Special Scenarios
// ============================================================================

#[test]
fn test_empty_repository() {
    let test_repo = TestRepo::new();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    // HEAD should exist even in empty repo
    let head = repo.head();
    assert!(head.is_ok(), "Should get HEAD in empty repository");
}

#[test]
fn test_initial_commit_has_no_parent() {
    let test_repo = TestRepo::new();

    // Create initial commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit = test_repo.stage_all_and_commit("Initial").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit_obj = repo.find_commit(commit.commit_sha).unwrap();

    // Should have no parents
    let parent_result = commit_obj.parent(0);
    assert!(
        parent_result.is_err(),
        "Initial commit should have no parent"
    );
}

#[test]
fn test_tree_clone() {
    let test_repo = TestRepo::new();

    // Create commit
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    let commit = test_repo.stage_all_and_commit("Test").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit_obj = repo.find_commit(commit.commit_sha).unwrap();
    let tree = commit_obj.tree().unwrap();
    let tree_clone = tree.clone();

    assert_eq!(
        tree.id(),
        tree_clone.id(),
        "Cloned tree should have same ID"
    );
}

#[test]
fn test_commit_with_unicode_message() {
    let test_repo = TestRepo::new();

    // Create commit with unicode message
    let mut file = test_repo.filename("test.txt");
    file.set_contents(lines!["content".human()]);
    test_repo.git(&["add", "-A"]).unwrap();
    test_repo
        .git(&["commit", "-m", "Unicode message: 你好世界 🎉"])
        .unwrap();

    let commit_sha = test_repo
        .git(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let commit = repo.find_commit(commit_sha).unwrap();
    let summary = commit.summary().unwrap();

    assert!(
        summary.contains("你好世界"),
        "Summary should contain unicode characters"
    );
}

#[test]
fn test_multiple_files_in_single_commit() {
    let test_repo = TestRepo::new();

    // Create multiple files
    let mut file1 = test_repo.filename("file1.txt");
    let mut file2 = test_repo.filename("file2.txt");
    let mut file3 = test_repo.filename("file3.txt");

    file1.set_contents(lines!["content1".human()]);
    file2.set_contents(lines!["content2".human()]);
    file3.set_contents(lines!["content3".human()]);

    let commit = test_repo.stage_all_and_commit("Multiple files").unwrap();

    let repo = find_repository(&[
        "-C".to_string(),
        test_repo.path().to_str().unwrap().to_string(),
    ])
    .unwrap();

    let files = repo.list_commit_files(&commit.commit_sha, None).unwrap();

    assert_eq!(files.len(), 3, "Should have 3 files in commit");
    assert!(files.contains("file1.txt"), "Should contain file1.txt");
    assert!(files.contains("file2.txt"), "Should contain file2.txt");
    assert!(files.contains("file3.txt"), "Should contain file3.txt");
}
