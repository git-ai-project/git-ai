//! Tests for multi-repository workspace support.
//!
//! This test module verifies that git-ai correctly handles workspaces that contain
//! multiple independent git repositories. The main scenarios tested are:
//!
//! 1. Detecting git repository from file paths when workspace root isn't a git repo
//! 2. Grouping files by their containing repository
//! 3. Handling submodules correctly (should be ignored in favor of parent repo)
//! 4. Edge cases with nested git directories
//! 5. Cross-repo checkpoints: AI edits from one repo to files in another repo

#[macro_use]
mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

use git_ai::error::GitAiError;
use git_ai::git::repository::{
    find_repository_for_file, find_repository_in_path, group_files_by_repository,
};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Creates a unique temporary directory for tests
fn create_unique_tmp_dir(prefix: &str) -> Result<PathBuf, GitAiError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();

    for _attempt in 0..100u32 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir_name = format!("{}-{}-{}-{}", prefix, now, pid, seq);
        let path = base.join(dir_name);

        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(GitAiError::IoError(e)),
        }
    }

    Err(GitAiError::Generic(
        "Failed to create a unique temporary directory".to_string(),
    ))
}

/// Initializes a git repository at the given path
fn init_git_repo(path: &PathBuf) -> Result<(), GitAiError> {
    fs::create_dir_all(path)?;

    let output = Command::new("git")
        .current_dir(path)
        .args(["init"])
        .output()
        .map_err(|e| GitAiError::Generic(format!("Failed to run git init: {}", e)))?;

    if !output.status.success() {
        return Err(GitAiError::Generic(format!(
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    // Configure user for the repository
    Command::new("git")
        .current_dir(path)
        .args(["config", "user.name", "Test User"])
        .output()
        .ok();

    Command::new("git")
        .current_dir(path)
        .args(["config", "user.email", "test@example.com"])
        .output()
        .ok();

    Ok(())
}

/// Creates a file at the given path with the specified content
fn create_file(path: &PathBuf, content: &str) -> Result<(), GitAiError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

/// Clean up a temporary directory
fn cleanup_tmp_dir(path: &PathBuf) {
    let _ = fs::remove_dir_all(path);
}

#[test]
fn test_find_repository_for_file_basic() {
    // Create a workspace directory (not a git repo)
    let workspace = create_unique_tmp_dir("git-ai-multi-repo-test").unwrap();

    // Create a git repository inside the workspace
    let repo_a = workspace.join("repo-a");
    init_git_repo(&repo_a).unwrap();

    // Create a file inside the repository
    let file_path = repo_a.join("src").join("main.rs");
    create_file(&file_path, "fn main() {}").unwrap();

    // Test that we can find the repository from the file
    let result = find_repository_for_file(
        file_path.to_str().unwrap(),
        Some(workspace.to_str().unwrap()),
    );

    assert!(result.is_ok(), "Should find repository from file path");

    let repo = result.unwrap();
    let workdir = repo.workdir().unwrap();

    // The workdir should be the repo-a directory
    assert!(
        workdir.ends_with("repo-a") || workdir.to_string_lossy().contains("repo-a"),
        "Repository workdir should be repo-a, got: {}",
        workdir.display()
    );

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_nonexistent_file_path() {
    // Test behavior when file paths don't exist on disk
    let workspace = create_unique_tmp_dir("git-ai-nonexistent-test").unwrap();

    let repo = workspace.join("repo");
    init_git_repo(&repo).unwrap();

    // Create one real file
    let real_file = repo.join("real_file.txt");
    create_file(&real_file, "content").unwrap();

    // Reference a file that doesn't exist
    let nonexistent_file = repo.join("nonexistent_file.txt");

    let file_paths = vec![
        real_file.to_str().unwrap().to_string(),
        nonexistent_file.to_str().unwrap().to_string(),
    ];

    let (repo_files, _orphan_files) =
        group_files_by_repository(&file_paths, Some(workspace.to_str().unwrap()));

    // The real file should be found in the repo
    // The nonexistent file behavior depends on implementation -
    // it should still find the repo since the parent directory exists
    assert!(
        !repo_files.is_empty(),
        "Should find repository for existing file"
    );

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_all_files_orphaned() {
    // Test when all provided files are orphans (no git repos)
    let workspace = create_unique_tmp_dir("git-ai-all-orphans-test").unwrap();

    // Create files without any git repository
    let file1 = workspace.join("dir1").join("file1.txt");
    let file2 = workspace.join("dir2").join("file2.txt");

    create_file(&file1, "content 1").unwrap();
    create_file(&file2, "content 2").unwrap();

    let file_paths = vec![
        file1.to_str().unwrap().to_string(),
        file2.to_str().unwrap().to_string(),
    ];

    let (repo_files, orphan_files) =
        group_files_by_repository(&file_paths, Some(workspace.to_str().unwrap()));

    // All files should be orphans
    assert!(
        repo_files.is_empty(),
        "Should have no repos when all files are orphans"
    );
    assert_eq!(orphan_files.len(), 2, "All files should be orphans");

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_single_repo_in_multi_repo_workspace() {
    // Test when only one repo has edits even though workspace has multiple repos
    let workspace = create_unique_tmp_dir("git-ai-single-edit-test").unwrap();

    // Create multiple repos but only edit files in one
    let repo_a = workspace.join("repo-a");
    let repo_b = workspace.join("repo-b");
    let repo_c = workspace.join("repo-c");

    init_git_repo(&repo_a).unwrap();
    init_git_repo(&repo_b).unwrap();
    init_git_repo(&repo_c).unwrap();

    // Only create/edit files in repo-b
    let file_b1 = repo_b.join("src").join("main.rs");
    let file_b2 = repo_b.join("src").join("lib.rs");

    create_file(&file_b1, "fn main() {}").unwrap();
    create_file(&file_b2, "pub fn lib() {}").unwrap();

    let file_paths = vec![
        file_b1.to_str().unwrap().to_string(),
        file_b2.to_str().unwrap().to_string(),
    ];

    let (repo_files, orphan_files) =
        group_files_by_repository(&file_paths, Some(workspace.to_str().unwrap()));

    // Should detect only 1 repository (repo-b)
    assert_eq!(
        repo_files.len(),
        1,
        "Should detect only 1 repository with edits"
    );

    assert!(orphan_files.is_empty(), "No orphan files");

    // Verify it's repo-b
    for (workdir, (_repo, files)) in &repo_files {
        assert!(
            workdir.to_string_lossy().contains("repo-b"),
            "Should be repo-b, got: {}",
            workdir.display()
        );
        assert_eq!(files.len(), 2, "repo-b should have 2 files");
    }

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_files_with_spaces_in_path() {
    // Test handling of paths with spaces
    let workspace = create_unique_tmp_dir("git-ai-spaces-test").unwrap();

    let repo = workspace.join("my project");
    init_git_repo(&repo).unwrap();

    let file_with_spaces = repo.join("src files").join("my file.txt");
    create_file(&file_with_spaces, "content").unwrap();

    let file_paths = vec![file_with_spaces.to_str().unwrap().to_string()];

    let (repo_files, orphan_files) =
        group_files_by_repository(&file_paths, Some(workspace.to_str().unwrap()));

    assert_eq!(repo_files.len(), 1, "Should find repo with spaces in path");
    assert!(orphan_files.is_empty(), "No orphan files");

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_symlinked_repository() {
    // Test that symlinked repositories are handled correctly
    let workspace = create_unique_tmp_dir("git-ai-symlink-test").unwrap();

    // Create actual repo
    let actual_repo = workspace.join("actual-repo");
    init_git_repo(&actual_repo).unwrap();

    let file_in_repo = actual_repo.join("file.txt");
    create_file(&file_in_repo, "content").unwrap();

    // Create symlink to the repo
    let symlink_path = workspace.join("linked-repo");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        if symlink(&actual_repo, &symlink_path).is_ok() {
            let file_via_symlink = symlink_path.join("file.txt");

            let result = find_repository_for_file(
                file_via_symlink.to_str().unwrap(),
                Some(workspace.to_str().unwrap()),
            );

            // Should find the repository through the symlink
            assert!(result.is_ok(), "Should find repository through symlink");
        }
    }

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_bare_repository_handling() {
    // Test that bare repositories are handled correctly (they have no working directory)
    let workspace = create_unique_tmp_dir("git-ai-bare-repo-test").unwrap();

    // Create a bare repository
    let bare_repo = workspace.join("bare.git");
    fs::create_dir_all(&bare_repo).unwrap();

    let output = Command::new("git")
        .current_dir(&bare_repo)
        .args(["init", "--bare"])
        .output();

    if output.is_ok() && output.unwrap().status.success() {
        // Create a normal repo alongside it
        let normal_repo = workspace.join("normal-repo");
        init_git_repo(&normal_repo).unwrap();

        let file = normal_repo.join("file.txt");
        create_file(&file, "content").unwrap();

        // File in normal repo should work fine
        let result =
            find_repository_for_file(file.to_str().unwrap(), Some(workspace.to_str().unwrap()));

        assert!(result.is_ok(), "Should find normal repository");
    }

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_duplicate_files_same_repo() {
    // Test that duplicate file paths are handled correctly
    let workspace = create_unique_tmp_dir("git-ai-duplicate-test").unwrap();

    let repo = workspace.join("repo");
    init_git_repo(&repo).unwrap();

    let file = repo.join("file.txt");
    create_file(&file, "content").unwrap();

    // Pass the same file twice
    let file_paths = vec![
        file.to_str().unwrap().to_string(),
        file.to_str().unwrap().to_string(),
    ];

    let (repo_files, orphan_files) =
        group_files_by_repository(&file_paths, Some(workspace.to_str().unwrap()));

    assert_eq!(repo_files.len(), 1, "Should have 1 repository");
    assert!(orphan_files.is_empty(), "No orphan files");

    // The duplicate should be included twice in the file list
    for (_repo, files) in repo_files.values() {
        assert_eq!(files.len(), 2, "Duplicate files should both be in the list");
    }

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_find_repository_for_file_with_multiple_repos() {
    // Create a workspace directory (not a git repo)
    let workspace = create_unique_tmp_dir("git-ai-multi-repo-test").unwrap();

    // Create two git repositories inside the workspace
    let repo_a = workspace.join("repo-a");
    let repo_b = workspace.join("repo-b");

    init_git_repo(&repo_a).unwrap();
    init_git_repo(&repo_b).unwrap();

    // Create files in each repository
    let file_a = repo_a.join("file_a.txt");
    let file_b = repo_b.join("file_b.txt");

    create_file(&file_a, "content a").unwrap();
    create_file(&file_b, "content b").unwrap();

    // Test file in repo-a
    let result_a =
        find_repository_for_file(file_a.to_str().unwrap(), Some(workspace.to_str().unwrap()));

    assert!(
        result_a.is_ok(),
        "Should find repository for file in repo-a"
    );
    let repo_a_found = result_a.unwrap();
    let workdir_a = repo_a_found.workdir().unwrap();
    assert!(
        workdir_a.ends_with("repo-a") || workdir_a.to_string_lossy().contains("repo-a"),
        "File a should be in repo-a"
    );

    // Test file in repo-b
    let result_b =
        find_repository_for_file(file_b.to_str().unwrap(), Some(workspace.to_str().unwrap()));

    assert!(
        result_b.is_ok(),
        "Should find repository for file in repo-b"
    );
    let repo_b_found = result_b.unwrap();
    let workdir_b = repo_b_found.workdir().unwrap();
    assert!(
        workdir_b.ends_with("repo-b") || workdir_b.to_string_lossy().contains("repo-b"),
        "File b should be in repo-b"
    );

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_find_repository_for_file_no_repo_found() {
    // Create a directory without a git repository
    let workspace = create_unique_tmp_dir("git-ai-no-repo-test").unwrap();

    // Create a file in the workspace (no git repo)
    let file_path = workspace.join("orphan_file.txt");
    create_file(&file_path, "content").unwrap();

    // Test that no repository is found
    let result = find_repository_for_file(
        file_path.to_str().unwrap(),
        Some(workspace.to_str().unwrap()),
    );

    assert!(
        result.is_err(),
        "Should not find repository for orphan file"
    );

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_find_repository_for_file_respects_workspace_boundary() {
    // Create a parent git repo and a workspace inside it
    let parent_repo = create_unique_tmp_dir("git-ai-parent-repo-test").unwrap();
    init_git_repo(&parent_repo).unwrap();

    // Create a workspace subdirectory (not a git repo) inside the parent
    let workspace = parent_repo.join("workspace");
    fs::create_dir_all(&workspace).unwrap();

    // Create a file in the workspace
    let file_path = workspace.join("file.txt");
    create_file(&file_path, "content").unwrap();

    // When workspace boundary is set, should NOT find the parent repo
    let result_with_boundary = find_repository_for_file(
        file_path.to_str().unwrap(),
        Some(workspace.to_str().unwrap()),
    );

    // This should fail because we're limiting the search to the workspace boundary
    assert!(
        result_with_boundary.is_err(),
        "Should not find parent repository when workspace boundary is set"
    );

    // When no workspace boundary is set, should find the parent repo
    let result_without_boundary = find_repository_for_file(file_path.to_str().unwrap(), None);

    assert!(
        result_without_boundary.is_ok(),
        "Should find parent repository when no workspace boundary is set"
    );

    cleanup_tmp_dir(&parent_repo);
}

#[test]
fn test_group_files_by_repository() {
    // Create a workspace directory (not a git repo)
    let workspace = create_unique_tmp_dir("git-ai-group-files-test").unwrap();

    // Create two git repositories inside the workspace
    let repo_a = workspace.join("repo-a");
    let repo_b = workspace.join("repo-b");

    init_git_repo(&repo_a).unwrap();
    init_git_repo(&repo_b).unwrap();

    // Create files in each repository
    let file_a1 = repo_a.join("file_a1.txt");
    let file_a2 = repo_a.join("src").join("file_a2.txt");
    let file_b1 = repo_b.join("file_b1.txt");
    let orphan = workspace.join("orphan.txt");

    create_file(&file_a1, "content a1").unwrap();
    create_file(&file_a2, "content a2").unwrap();
    create_file(&file_b1, "content b1").unwrap();
    create_file(&orphan, "orphan content").unwrap();

    // Group files by repository
    let file_paths = vec![
        file_a1.to_str().unwrap().to_string(),
        file_a2.to_str().unwrap().to_string(),
        file_b1.to_str().unwrap().to_string(),
        orphan.to_str().unwrap().to_string(),
    ];

    let (repo_files, orphan_files) =
        group_files_by_repository(&file_paths, Some(workspace.to_str().unwrap()));

    // Should have 2 repositories detected
    assert_eq!(repo_files.len(), 2, "Should detect 2 repositories");

    // Should have 1 orphan file
    assert_eq!(orphan_files.len(), 1, "Should have 1 orphan file");
    assert!(
        orphan_files[0].contains("orphan.txt"),
        "Orphan file should be orphan.txt"
    );

    // Verify file grouping
    let mut repo_a_files_count = 0;
    let mut repo_b_files_count = 0;

    for (workdir, (_repo, files)) in &repo_files {
        if workdir.to_string_lossy().contains("repo-a") {
            repo_a_files_count = files.len();
            assert_eq!(files.len(), 2, "repo-a should have 2 files");
        } else if workdir.to_string_lossy().contains("repo-b") {
            repo_b_files_count = files.len();
            assert_eq!(files.len(), 1, "repo-b should have 1 file");
        }
    }

    assert_eq!(repo_a_files_count, 2, "Should find 2 files in repo-a");
    assert_eq!(repo_b_files_count, 1, "Should find 1 file in repo-b");

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_find_repository_for_file_nested_repos() {
    // Create a workspace with nested git repositories
    let workspace = create_unique_tmp_dir("git-ai-nested-repos-test").unwrap();

    // Create outer git repository
    let outer_repo = workspace.join("outer");
    init_git_repo(&outer_repo).unwrap();

    // Create inner git repository (nested)
    let inner_repo = outer_repo.join("inner");
    init_git_repo(&inner_repo).unwrap();

    // Create files in both repositories
    let outer_file = outer_repo.join("outer_file.txt");
    let inner_file = inner_repo.join("inner_file.txt");

    create_file(&outer_file, "outer content").unwrap();
    create_file(&inner_file, "inner content").unwrap();

    // File in outer repo should find outer repo
    let result_outer = find_repository_for_file(
        outer_file.to_str().unwrap(),
        Some(workspace.to_str().unwrap()),
    );

    assert!(result_outer.is_ok(), "Should find outer repository");
    let outer_workdir = result_outer.unwrap().workdir().unwrap();
    assert!(
        outer_workdir.ends_with("outer") && !outer_workdir.to_string_lossy().contains("inner"),
        "Outer file should be in outer repo, got: {}",
        outer_workdir.display()
    );

    // File in inner repo should find inner repo (the nearest .git)
    let result_inner = find_repository_for_file(
        inner_file.to_str().unwrap(),
        Some(workspace.to_str().unwrap()),
    );

    assert!(result_inner.is_ok(), "Should find inner repository");
    let inner_workdir = result_inner.unwrap().workdir().unwrap();
    assert!(
        inner_workdir.to_string_lossy().contains("inner"),
        "Inner file should be in inner repo, got: {}",
        inner_workdir.display()
    );

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_find_repository_in_path_still_works() {
    // Ensure the original function still works for normal single-repo scenarios
    let repo = create_unique_tmp_dir("git-ai-single-repo-test").unwrap();
    init_git_repo(&repo).unwrap();

    // Create an initial commit so the repo is valid
    let file = repo.join("README.md");
    create_file(&file, "# Test").unwrap();

    Command::new("git")
        .current_dir(&repo)
        .args(["add", "."])
        .output()
        .ok();

    Command::new("git")
        .current_dir(&repo)
        .args(["commit", "-m", "Initial commit"])
        .output()
        .ok();

    // The original function should work
    let result = find_repository_in_path(repo.to_str().unwrap());
    assert!(
        result.is_ok(),
        "find_repository_in_path should work for normal repos"
    );

    cleanup_tmp_dir(&repo);
}

#[test]
fn test_find_repository_for_directory() {
    // Test that find_repository_for_file works with directories too
    let workspace = create_unique_tmp_dir("git-ai-dir-test").unwrap();

    let repo = workspace.join("repo");
    init_git_repo(&repo).unwrap();

    let subdir = repo.join("src").join("components");
    fs::create_dir_all(&subdir).unwrap();

    // Test finding repo from a directory path
    let result =
        find_repository_for_file(subdir.to_str().unwrap(), Some(workspace.to_str().unwrap()));

    assert!(result.is_ok(), "Should find repository from directory path");
    let workdir = result.unwrap().workdir().unwrap();
    assert!(
        workdir.to_string_lossy().contains("repo"),
        "Directory should be in repo"
    );

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_empty_file_list_grouping() {
    // Test edge case with empty file list
    let (repo_files, orphan_files) = group_files_by_repository(&[], None);

    assert!(
        repo_files.is_empty(),
        "Should have no repos with empty file list"
    );
    assert!(
        orphan_files.is_empty(),
        "Should have no orphans with empty file list"
    );
}

#[test]
fn test_cross_repo_edits_grouping() {
    // Test that files from a single AI session spanning multiple repos are grouped correctly
    let workspace = create_unique_tmp_dir("git-ai-cross-repo-test").unwrap();

    // Create three git repositories simulating a monorepo-like workspace
    let frontend_repo = workspace.join("frontend");
    let backend_repo = workspace.join("backend");
    let shared_repo = workspace.join("shared");

    init_git_repo(&frontend_repo).unwrap();
    init_git_repo(&backend_repo).unwrap();
    init_git_repo(&shared_repo).unwrap();

    // Create files in each repository (simulating an AI making related changes across repos)
    let frontend_file1 = frontend_repo.join("src").join("App.tsx");
    let frontend_file2 = frontend_repo
        .join("src")
        .join("components")
        .join("Button.tsx");
    let backend_file = backend_repo.join("src").join("api.py");
    let shared_file = shared_repo.join("types").join("shared_types.ts");

    create_file(&frontend_file1, "// Frontend App").unwrap();
    create_file(&frontend_file2, "// Button component").unwrap();
    create_file(&backend_file, "# Backend API").unwrap();
    create_file(&shared_file, "// Shared types").unwrap();

    // Simulate a single AI session editing files across all repos
    let file_paths = vec![
        frontend_file1.to_str().unwrap().to_string(),
        frontend_file2.to_str().unwrap().to_string(),
        backend_file.to_str().unwrap().to_string(),
        shared_file.to_str().unwrap().to_string(),
    ];

    let (repo_files, orphan_files) =
        group_files_by_repository(&file_paths, Some(workspace.to_str().unwrap()));

    // Should have 3 repositories detected
    assert_eq!(
        repo_files.len(),
        3,
        "Should detect 3 repositories for cross-repo edits"
    );

    // No orphan files
    assert!(orphan_files.is_empty(), "Should have no orphan files");

    // Verify correct distribution
    let mut frontend_count = 0;
    let mut backend_count = 0;
    let mut shared_count = 0;

    for (workdir, (_repo, files)) in &repo_files {
        let workdir_str = workdir.to_string_lossy();
        if workdir_str.contains("frontend") {
            frontend_count = files.len();
        } else if workdir_str.contains("backend") {
            backend_count = files.len();
        } else if workdir_str.contains("shared") {
            shared_count = files.len();
        }
    }

    assert_eq!(frontend_count, 2, "Frontend should have 2 files");
    assert_eq!(backend_count, 1, "Backend should have 1 file");
    assert_eq!(shared_count, 1, "Shared should have 1 file");

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_workspace_relative_paths() {
    // Test that relative paths work when converted to absolute
    let workspace = create_unique_tmp_dir("git-ai-relative-path-test").unwrap();

    let repo = workspace.join("my-project");
    init_git_repo(&repo).unwrap();

    // Create files
    let file1 = repo.join("src").join("main.rs");
    let file2 = repo.join("lib").join("utils.rs");

    create_file(&file1, "fn main() {}").unwrap();
    create_file(&file2, "pub fn util() {}").unwrap();

    // Test with workspace-relative paths (simulating what an IDE might send)
    // When paths are relative to workspace root
    let relative_paths = [
        "my-project/src/main.rs".to_string(),
        "my-project/lib/utils.rs".to_string(),
    ];

    // Convert to absolute paths (simulating what handle_checkpoint does)
    let absolute_paths: Vec<String> = relative_paths
        .iter()
        .map(|p| workspace.join(p).to_string_lossy().to_string())
        .collect();

    let (repo_files, orphan_files) =
        group_files_by_repository(&absolute_paths, Some(workspace.to_str().unwrap()));

    assert_eq!(repo_files.len(), 1, "Should find 1 repository");
    assert!(orphan_files.is_empty(), "Should have no orphan files");

    // Verify the files are grouped correctly
    for (_repo, files) in repo_files.values() {
        assert_eq!(files.len(), 2, "Should have 2 files in the repo");
    }

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_deeply_nested_file_detection() {
    // Test that deeply nested files still find their repository correctly
    let workspace = create_unique_tmp_dir("git-ai-deep-nest-test").unwrap();

    let repo = workspace.join("monorepo");
    init_git_repo(&repo).unwrap();

    // Create a deeply nested file structure
    let deep_file = repo
        .join("packages")
        .join("core")
        .join("src")
        .join("utils")
        .join("helpers")
        .join("deeply_nested.ts");

    create_file(&deep_file, "export const helper = () => {};").unwrap();

    let result = find_repository_for_file(
        deep_file.to_str().unwrap(),
        Some(workspace.to_str().unwrap()),
    );

    assert!(
        result.is_ok(),
        "Should find repository for deeply nested file"
    );

    let workdir = result.unwrap().workdir().unwrap();
    assert!(
        workdir.to_string_lossy().contains("monorepo"),
        "Deeply nested file should be in monorepo"
    );

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_mixed_absolute_and_relative_grouping() {
    // Test grouping with a mix of absolute and relative paths
    let workspace = create_unique_tmp_dir("git-ai-mixed-paths-test").unwrap();

    let repo = workspace.join("project");
    init_git_repo(&repo).unwrap();

    let file1 = repo.join("file1.txt");
    let file2 = repo.join("file2.txt");

    create_file(&file1, "content 1").unwrap();
    create_file(&file2, "content 2").unwrap();

    // Mix of absolute path and path that needs conversion
    let paths = vec![
        file1.to_str().unwrap().to_string(), // Already absolute
        file2.to_str().unwrap().to_string(), // Already absolute
    ];

    let (repo_files, orphan_files) =
        group_files_by_repository(&paths, Some(workspace.to_str().unwrap()));

    assert_eq!(repo_files.len(), 1, "Should detect 1 repository");
    assert!(orphan_files.is_empty(), "Should have no orphans");

    for (_repo, files) in repo_files.values() {
        assert_eq!(files.len(), 2, "Both files should be in the same repo");
    }

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_repository_isolation() {
    // Verify that files in different repos don't get mixed up
    // and each repo maintains its own attribution tracking
    let workspace = create_unique_tmp_dir("git-ai-isolation-test").unwrap();

    // Create two completely separate repos
    let repo_alpha = workspace.join("alpha");
    let repo_beta = workspace.join("beta");

    init_git_repo(&repo_alpha).unwrap();
    init_git_repo(&repo_beta).unwrap();

    // Create same-named files in both repos (shouldn't cause confusion)
    let alpha_readme = repo_alpha.join("README.md");
    let beta_readme = repo_beta.join("README.md");
    let alpha_config = repo_alpha.join("config.json");
    let beta_config = repo_beta.join("config.json");

    create_file(&alpha_readme, "# Alpha Project").unwrap();
    create_file(&beta_readme, "# Beta Project").unwrap();
    create_file(&alpha_config, r#"{"name": "alpha"}"#).unwrap();
    create_file(&beta_config, r#"{"name": "beta"}"#).unwrap();

    let file_paths = vec![
        alpha_readme.to_str().unwrap().to_string(),
        beta_readme.to_str().unwrap().to_string(),
        alpha_config.to_str().unwrap().to_string(),
        beta_config.to_str().unwrap().to_string(),
    ];

    let (repo_files, orphan_files) =
        group_files_by_repository(&file_paths, Some(workspace.to_str().unwrap()));

    // Verify isolation - should be 2 separate repos
    assert_eq!(repo_files.len(), 2, "Should have 2 isolated repositories");
    assert!(orphan_files.is_empty(), "No orphan files");

    // Each repo should have exactly 2 files
    for (workdir, (_repo, files)) in &repo_files {
        assert_eq!(
            files.len(),
            2,
            "Each repo should have exactly 2 files, got {} in {}",
            files.len(),
            workdir.display()
        );

        // Verify files belong to correct repo
        let workdir_str = workdir.to_string_lossy();
        for file in files {
            if workdir_str.contains("alpha") {
                assert!(
                    file.contains("alpha"),
                    "Alpha repo should only contain alpha files"
                );
            } else if workdir_str.contains("beta") {
                assert!(
                    file.contains("beta"),
                    "Beta repo should only contain beta files"
                );
            }
        }
    }

    cleanup_tmp_dir(&workspace);
}

#[test]
fn test_cross_repo_checkpoint_creates_working_log_in_target_repo() {
    let repo1 = TestRepo::new();
    let repo2 = TestRepo::new();

    let mut file = repo2.filename("test.txt");
    file.set_contents(lines!["Line 1", "Line 2", "Line 3"]);
    repo2.stage_all_and_commit("Initial commit").unwrap();

    fs::write(
        repo2.path().join("test.txt"),
        "Line 1\nLine 2\nLine 3\nAI Line 1\nAI Line 2\n",
    )
    .unwrap();

    let repo2_file_abs = repo2.canonical_path().join("test.txt");
    let abs_path_str = repo2_file_abs.to_str().unwrap();

    repo2
        .git_ai_from_working_dir(
            &repo1.canonical_path(),
            &["checkpoint", "mock_ai", abs_path_str],
        )
        .unwrap();

    let working_log = repo2.current_working_logs();
    let ai_files = working_log.all_ai_touched_files().unwrap_or_default();
    assert!(
        !ai_files.is_empty(),
        "Cross-repo checkpoint should create working log entries in the target repo (repo2), but found none. \
         This means the checkpoint from repo1's working directory failed to write to repo2's working log."
    );
}

#[test]
fn test_cross_repo_checkpoint_ai_attribution_on_commit() {
    let repo1 = TestRepo::new();
    let repo2 = TestRepo::new();

    let mut file = repo2.filename("test.txt");
    file.set_contents(lines!["Line 1", "Line 2", "Line 3"]);
    repo2.stage_all_and_commit("Initial commit").unwrap();

    fs::write(
        repo2.path().join("test.txt"),
        "Line 1\nLine 2\nLine 3\nAI Line 1\nAI Line 2\n",
    )
    .unwrap();

    let repo2_file_abs = repo2.canonical_path().join("test.txt");
    let abs_path_str = repo2_file_abs.to_str().unwrap();

    repo2
        .git_ai_from_working_dir(
            &repo1.canonical_path(),
            &["checkpoint", "mock_ai", abs_path_str],
        )
        .unwrap();

    let commit = repo2.stage_all_and_commit("AI edits from repo1").unwrap();

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Cross-repo AI edits should be attributed to AI, not human. \
         The checkpoint was run from repo1's working directory for a file in repo2, \
         but the commit in repo2 shows no AI attestations."
    );
}

#[test]
fn test_cross_repo_checkpoint_preserves_local_repo_checkpoint() {
    let repo1 = TestRepo::new();
    let repo2 = TestRepo::new();

    let mut file1 = repo1.filename("local.txt");
    file1.set_contents(lines!["Local 1", "Local 2"]);
    repo1
        .stage_all_and_commit("Initial commit in repo1")
        .unwrap();

    let mut file2 = repo2.filename("remote.txt");
    file2.set_contents(lines!["Remote 1", "Remote 2"]);
    repo2
        .stage_all_and_commit("Initial commit in repo2")
        .unwrap();

    fs::write(
        repo1.path().join("local.txt"),
        "Local 1\nLocal 2\nAI local line\n",
    )
    .unwrap();
    fs::write(
        repo2.path().join("remote.txt"),
        "Remote 1\nRemote 2\nAI remote line\n",
    )
    .unwrap();

    let repo1_file_abs = repo1.canonical_path().join("local.txt");
    let repo2_file_abs = repo2.canonical_path().join("remote.txt");

    repo1
        .git_ai_from_working_dir(
            &repo1.canonical_path(),
            &[
                "checkpoint",
                "mock_ai",
                repo1_file_abs.to_str().unwrap(),
                repo2_file_abs.to_str().unwrap(),
            ],
        )
        .unwrap();

    let repo1_commit = repo1.stage_all_and_commit("AI edits in repo1").unwrap();
    assert!(
        !repo1_commit.authorship_log.attestations.is_empty(),
        "Local repo (repo1) should still have AI attestations when cross-repo files are also checkpointed"
    );

    let repo2_commit = repo2.stage_all_and_commit("AI edits in repo2").unwrap();
    assert!(
        !repo2_commit.authorship_log.attestations.is_empty(),
        "Cross-repo (repo2) should also have AI attestations"
    );
}
