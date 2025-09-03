use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_staged_only_flag_ignores_unstaged_changes() {
    let temp_dir = TempDir::new().unwrap();
    let repo_path = temp_dir.path();

    // Initialize git repo
    Command::new("git")
        .args(&["init"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Configure git user
    Command::new("git")
        .args(&["config", "user.name", "Test User"])
        .current_dir(repo_path)
        .assert()
        .success();

    Command::new("git")
        .args(&["config", "user.email", "test@example.com"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Create initial commit
    fs::write(repo_path.join("initial.txt"), "initial content").unwrap();
    Command::new("git")
        .args(&["add", "initial.txt"])
        .current_dir(repo_path)
        .assert()
        .success();

    Command::new("git")
        .args(&["commit", "-m", "Initial commit"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Create staged file
    fs::write(repo_path.join("staged.txt"), "staged content").unwrap();
    Command::new("git")
        .args(&["add", "staged.txt"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Create unstaged file
    fs::write(repo_path.join("unstaged.txt"), "unstaged content").unwrap();

    // Test --staged-only flag
    let mut cmd = Command::cargo_bin("git-ai").unwrap();
    cmd.args(&["checkpoint", "--author", "Claude", "--staged-only"])
        .current_dir(repo_path)
        .assert()
        .success()
        .stderr(predicate::str::contains("Warning: Found 1 unstaged file(s) that will be ignored"))
        .stderr(predicate::str::contains("unstaged.txt"));
}

#[test]
fn test_without_staged_only_includes_all_changes() {
    let temp_dir = TempDir::new().unwrap();
    let repo_path = temp_dir.path();

    // Initialize git repo
    Command::new("git")
        .args(&["init"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Configure git user
    Command::new("git")
        .args(&["config", "user.name", "Test User"])
        .current_dir(repo_path)
        .assert()
        .success();

    Command::new("git")
        .args(&["config", "user.email", "test@example.com"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Create initial commit
    fs::write(repo_path.join("initial.txt"), "initial content").unwrap();
    Command::new("git")
        .args(&["add", "initial.txt"])
        .current_dir(repo_path)
        .assert()
        .success();

    Command::new("git")
        .args(&["commit", "-m", "Initial commit"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Create staged file
    fs::write(repo_path.join("staged.txt"), "staged content").unwrap();
    Command::new("git")
        .args(&["add", "staged.txt"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Create unstaged file
    fs::write(repo_path.join("unstaged.txt"), "unstaged content").unwrap();

    // Test without --staged-only flag (should include both files)
    let mut cmd = Command::cargo_bin("git-ai").unwrap();
    cmd.args(&["checkpoint", "--author", "Claude"])
        .current_dir(repo_path)
        .assert()
        .success()
        .stderr(predicate::str::contains("Claude changed 2 of the 2 file(s)"));
}

#[test]
fn test_staged_only_with_no_unstaged_changes() {
    let temp_dir = TempDir::new().unwrap();
    let repo_path = temp_dir.path();

    // Initialize git repo
    Command::new("git")
        .args(&["init"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Configure git user
    Command::new("git")
        .args(&["config", "user.name", "Test User"])
        .current_dir(repo_path)
        .assert()
        .success();

    Command::new("git")
        .args(&["config", "user.email", "test@example.com"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Create initial commit
    fs::write(repo_path.join("initial.txt"), "initial content").unwrap();
    Command::new("git")
        .args(&["add", "initial.txt"])
        .current_dir(repo_path)
        .assert()
        .success();

    Command::new("git")
        .args(&["commit", "-m", "Initial commit"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Create only staged file
    fs::write(repo_path.join("staged.txt"), "staged content").unwrap();
    Command::new("git")
        .args(&["add", "staged.txt"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Test --staged-only flag with no unstaged changes (should not show warning)
    let mut cmd = Command::cargo_bin("git-ai").unwrap();
    cmd.args(&["checkpoint", "--author", "Claude", "--staged-only"])
        .current_dir(repo_path)
        .assert()
        .success()
        .stderr(predicate::str::contains("Warning").not());
}

#[test]
fn test_staged_only_with_mixed_file_types() {
    let temp_dir = TempDir::new().unwrap();
    let repo_path = temp_dir.path();

    // Initialize git repo
    Command::new("git")
        .args(&["init"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Configure git user
    Command::new("git")
        .args(&["config", "user.name", "Test User"])
        .current_dir(repo_path)
        .assert()
        .success();

    Command::new("git")
        .args(&["config", "user.email", "test@example.com"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Create initial commit
    fs::write(repo_path.join("initial.txt"), "initial content").unwrap();
    Command::new("git")
        .args(&["add", "initial.txt"])
        .current_dir(repo_path)
        .assert()
        .success();

    Command::new("git")
        .args(&["commit", "-m", "Initial commit"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Create staged modification
    fs::write(repo_path.join("initial.txt"), "modified staged content").unwrap();
    Command::new("git")
        .args(&["add", "initial.txt"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Create unstaged modification on top
    fs::write(repo_path.join("initial.txt"), "modified staged content\nplus unstaged").unwrap();

    // Create new staged file
    fs::write(repo_path.join("new_staged.txt"), "new staged file").unwrap();
    Command::new("git")
        .args(&["add", "new_staged.txt"])
        .current_dir(repo_path)
        .assert()
        .success();

    // Create new unstaged file
    fs::write(repo_path.join("new_unstaged.txt"), "new unstaged file").unwrap();

    // Test --staged-only flag should only track staged changes
    let mut cmd = Command::cargo_bin("git-ai").unwrap();
    cmd.args(&["checkpoint", "--author", "Claude", "--staged-only"])
        .current_dir(repo_path)
        .assert()
        .success()
        .stderr(predicate::str::contains("Warning: Found 2 unstaged file(s) that will be ignored"))
        .stderr(predicate::str::contains("initial.txt"))
        .stderr(predicate::str::contains("new_unstaged.txt"));
}