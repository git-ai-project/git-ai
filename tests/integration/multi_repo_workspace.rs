use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

// =============================================================================
// Cross-repo checkpoint attributes correct repo
// =============================================================================

#[test]
fn test_cross_repo_checkpoint_attributes_correct_repo() {
    let repo1 = TestRepo::new();
    let repo2 = TestRepo::new();

    // Set up repo2 with an initial file
    let mut file = repo2.filename("target.txt");
    file.set_contents(crate::lines!["Line 1", "Line 2", "Line 3"]);
    repo2
        .stage_all_and_commit("Initial commit in repo2")
        .unwrap();

    // Simulate AI editing a file in repo2
    fs::write(
        repo2.path().join("target.txt"),
        "Line 1\nLine 2\nLine 3\nAI Line 1\nAI Line 2\n",
    )
    .unwrap();

    // Run checkpoint from repo1's working directory but targeting repo2's file
    let repo2_file_abs = repo2.canonical_path().join("target.txt");
    let abs_path_str = repo2_file_abs.to_str().unwrap();

    repo2
        .git_ai_from_working_dir(
            &repo1.canonical_path(),
            &["checkpoint", "mock_ai", abs_path_str],
        )
        .unwrap();

    // Commit in repo2 -- should have AI attribution
    let commit = repo2.stage_all_and_commit("AI edits from repo1").unwrap();

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Cross-repo checkpoint should result in AI attestations in the target repo"
    );
}

// =============================================================================
// Files in separate repos are independent
// =============================================================================

#[test]
fn test_files_in_separate_repos_independent() {
    let repo1 = TestRepo::new();
    let repo2 = TestRepo::new();

    // Set up both repos with initial commits
    let mut file1 = repo1.filename("shared_name.txt");
    file1.set_contents(crate::lines!["Repo1 content"]);
    repo1.stage_all_and_commit("Initial in repo1").unwrap();

    let mut file2 = repo2.filename("shared_name.txt");
    file2.set_contents(crate::lines!["Repo2 content"]);
    repo2.stage_all_and_commit("Initial in repo2").unwrap();

    // AI edits in repo1 only
    let mut ai_file1 = repo1.filename("ai_output.py");
    ai_file1.set_contents(crate::lines!["def func_a():".ai(), "    return 'a'".ai(),]);
    repo1.stage_all_and_commit("AI edit in repo1").unwrap();

    // Human edits in repo2 only
    let file2_path = repo2.path().join("human_edit.py");
    fs::write(&file2_path, "def func_b():\n    return 'b'\n").unwrap();
    repo2
        .git_ai(&["checkpoint", "mock_known_human", "human_edit.py"])
        .unwrap();
    repo2.stage_all_and_commit("Human edit in repo2").unwrap();

    // Verify repo1 has AI attribution
    ai_file1.assert_lines_and_blame(crate::lines!["def func_a():".ai(), "    return 'a'".ai(),]);

    // Verify repo2 does NOT have AI attribution (human edit)
    let blame2 = repo2.git_ai(&["blame", "human_edit.py"]).unwrap();
    assert!(
        !blame2.contains("mock_ai"),
        "Repo2's human edit should not be attributed to AI. Got:\n{}",
        blame2
    );
}

// =============================================================================
// Checkpoint with absolute path resolves to correct repo
// =============================================================================

#[test]
fn test_checkpoint_with_absolute_path() {
    let repo = TestRepo::new();

    // Initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Write a file using absolute path
    let abs_file_path = repo.path().join("src").join("main.rs");
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(&abs_file_path, "fn main() {}\nfn helper() {}\n").unwrap();

    // Checkpoint using the absolute path
    let abs_str = abs_file_path.to_str().unwrap();
    let result = repo.git_ai(&["checkpoint", "mock_ai", abs_str]);
    assert!(
        result.is_ok(),
        "Checkpoint with absolute path should succeed, got: {:?}",
        result.err()
    );

    // Commit and verify attribution
    repo.stage_all_and_commit("Add main.rs via absolute path")
        .unwrap();

    let blame = repo.git_ai(&["blame", "src/main.rs"]).unwrap();
    assert!(
        blame.contains("mock_ai"),
        "File checkpointed with absolute path should have AI attribution. Got:\n{}",
        blame
    );
}

crate::reuse_tests_in_worktree!(
    test_cross_repo_checkpoint_attributes_correct_repo,
    test_files_in_separate_repos_independent,
    test_checkpoint_with_absolute_path,
);
