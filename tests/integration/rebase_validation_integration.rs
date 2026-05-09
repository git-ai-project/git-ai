//! Integration tests for rebase pre-flight validation and post-rebase verification.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

#[test]
fn test_validation_passes_with_clean_state() {
    let repo = TestRepo::new();

    // Create proper setup with working logs
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\n").unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Feature branch with AI changes and proper checkpoints
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "line 1\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("AI work").unwrap();

    // Main branch advance - add a different line to avoid conflict
    repo.git(&["checkout", "main"]).unwrap();
    fs::write(&file_path, "main change\nline 1\n").unwrap();
    repo.stage_all_and_commit("Main work").unwrap();

    // Rebase should succeed without warnings
    repo.git(&["checkout", "feature"]).unwrap();
    let result = repo.git(&["rebase", "main"]);

    // Check that rebase succeeded
    assert!(result.is_ok(), "Rebase failed: {:?}", result);

    // Verify attribution survived
    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(lines![
        "main change".human(),
        "line 1".human(),
        "AI line".ai(),
    ]);
}

#[test]
fn test_rebase_verification_preserves_attribution() {
    let repo = TestRepo::new();

    // Create initial commit
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\n").unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Create branch with AI changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "line 1\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("AI commit").unwrap();

    // Verify AI line is attributed before rebase
    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(lines!["line 1".human(), "AI line".ai(),]);

    // Switch to main
    repo.git(&["checkout", "main"]).unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    // Verification should detect if attribution was preserved
    file.assert_committed_lines(lines!["line 1".human(), "AI line".ai(),]);
}
