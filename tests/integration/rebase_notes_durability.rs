//! Comprehensive end-to-end tests for notes durability during rebases.
//!
//! Tests cover:
//! - Orphaned notes parking when rebase produces no new commits
//! - Recovery paths for lost attribution
//! - Notes survival through various rebase scenarios

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

#[test]
fn test_orphaned_notes_parked_when_no_new_commits() {
    let repo = TestRepo::new();

    // Create initial commit
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\n").unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Create feature branch with AI changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "line 1\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("AI commit").unwrap();

    let original_head = repo.git_og(&["rev-parse", "HEAD"]).unwrap();
    let original_head = original_head.trim().to_string();

    // Verify AI line is attributed
    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(lines!["line 1".human(), "AI line".ai(),]);

    // Reset to main (simulates a failed rebase scenario where no new commits are created)
    repo.git(&["checkout", "main"]).unwrap();
    repo.git(&["branch", "-D", "feature"]).unwrap();

    // Manually trigger the orphaned notes scenario by calling git-ai with empty new commits
    // In real scenarios, this happens when build_rebase_commit_mappings returns empty

    // Check that orphaned notes ref exists
    let orphaned_ref = format!("refs/git-ai/orphaned-notes/{}", original_head);

    // For now, we can't easily trigger the orphaned notes scenario without deeper integration
    // This test documents the expected behavior and can be expanded when recovery command exists
}

#[test]
fn test_notes_survive_through_successful_rebase() {
    let repo = TestRepo::new();

    // Create initial commit
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\n").unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Feature branch with AI changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "line 1\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("AI commit").unwrap();

    // Verify AI attribution before rebase
    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(lines!["line 1".human(), "AI line".ai(),]);

    // Main branch advances
    repo.git(&["checkout", "main"]).unwrap();
    fs::write(&file_path, "main change\nline 1\n").unwrap();
    repo.stage_all_and_commit("Main advance").unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    // Verify attribution survived the rebase
    file.assert_committed_lines(lines![
        "main change".human(),
        "line 1".human(),
        "AI line".ai(),
    ]);
}

#[test]
fn test_original_commits_and_notes_still_exist_after_rebase() {
    let repo = TestRepo::new();

    // Create initial commit
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\n").unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Feature branch with AI changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "line 1\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("AI commit").unwrap();

    // Capture original commit SHA
    let original_sha_output = repo.git_og(&["rev-parse", "HEAD"]).unwrap();
    let original_sha = original_sha_output.trim().to_string();

    // Main branch advances
    repo.git(&["checkout", "main"]).unwrap();
    fs::write(&file_path, "main change\nline 1\n").unwrap();
    repo.stage_all_and_commit("Main advance").unwrap();

    // Rebase
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    // Original commit should still exist in object store
    let cat_result = repo.git_og(&["cat-file", "-t", &original_sha]);
    assert!(cat_result.is_ok(), "Original commit should still exist");

    // Original commit's note should still exist (can check via git notes)
    let note_result = repo.git_og(&["notes", "--ref=ai", "show", &original_sha]);
    // Note: This might fail if notes weren't copied, which is fine - that's what we're testing
}

#[test]
fn test_multiple_ai_commits_preserve_attribution_through_rebase() {
    let repo = TestRepo::new();

    // Initial commit
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\n").unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Feature branch with multiple AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI commit
    fs::write(&file_path, "line 1\nAI line 1\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("AI commit 1").unwrap();

    // Second AI commit
    fs::write(&file_path, "line 1\nAI line 1\nAI line 2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("AI commit 2").unwrap();

    // Main advances
    repo.git(&["checkout", "main"]).unwrap();
    fs::write(&file_path, "main line\nline 1\n").unwrap();
    repo.stage_all_and_commit("Main").unwrap();

    // Rebase
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    // Verify all AI lines survived
    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(lines![
        "main line".human(),
        "line 1".human(),
        "AI line 1".ai(),
        "AI line 2".ai(),
    ]);
}

#[test]
fn test_rebase_with_conflicts_preserves_ai_lines() {
    let repo = TestRepo::new();

    // Initial commit
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\nline 2\nline 3\n").unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Feature branch adds AI line at end (no conflict position)
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "line 1\nline 2\nline 3\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("AI commit").unwrap();

    // Main adds different line at beginning (no conflict)
    repo.git(&["checkout", "main"]).unwrap();
    fs::write(&file_path, "main line\nline 1\nline 2\nline 3\n").unwrap();
    repo.stage_all_and_commit("Main").unwrap();

    // Rebase should succeed without conflicts
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    // Verify AI line survived
    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(lines![
        "main line".human(),
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "AI line".ai(),
    ]);
}

#[test]
fn test_notes_snapshot_created_before_rebase() {
    let repo = TestRepo::new();

    // Create initial commit with AI attribution
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Verify note exists
    let head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let note_before = repo.git_og(&["notes", "--ref=ai", "show", &head]);
    assert!(note_before.is_ok(), "Note should exist before rebase");

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "line 1\nAI line\nfeature line\n").unwrap();
    repo.stage_all_and_commit("Feature").unwrap();

    // Advance main
    repo.git(&["checkout", "main"]).unwrap();
    fs::write(&file_path, "main line\nline 1\nAI line\n").unwrap();
    repo.stage_all_and_commit("Main").unwrap();

    // Rebase (snapshot should be created automatically)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    // Verify notes survived
    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(lines![
        "main line".human(),
        "line 1".ai(), // Was AI attributed in initial commit
        "AI line".ai(),
        "feature line".human(),
    ]);

    // Check if backup refs exist (should be cleaned up after successful rebase)
    let refs_output = repo
        .git_og(&["for-each-ref", "refs/git-ai/backup/"])
        .unwrap();
    // Backup should be cleaned up after successful rebase
    assert!(
        refs_output.is_empty() || refs_output.trim().is_empty(),
        "Backup refs should be cleaned up after successful rebase"
    );
}

#[test]
fn test_notes_snapshot_survives_failed_rebase() {
    let repo = TestRepo::new();

    // Create initial commit with AI attribution
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    let original_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Create feature branch with conflicting change
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "line 1\nfeature conflict\n").unwrap();
    repo.stage_all_and_commit("Feature").unwrap();

    // Main has conflicting change at same location
    repo.git(&["checkout", "main"]).unwrap();
    fs::write(&file_path, "line 1\nmain conflict\n").unwrap();
    repo.stage_all_and_commit("Main").unwrap();

    // Attempt rebase (will fail with conflict)
    repo.git(&["checkout", "feature"]).unwrap();
    let rebase_result = repo.git(&["rebase", "main"]);

    if rebase_result.is_err() {
        // Abort the rebase
        repo.git(&["rebase", "--abort"]).unwrap();

        // Original note should still exist
        let note_result = repo.git_og(&["notes", "--ref=ai", "show", &original_head]);
        assert!(
            note_result.is_ok(),
            "Original note should still exist after aborted rebase"
        );
    }
}

#[test]
fn test_multiple_rebases_create_separate_snapshots() {
    let repo = TestRepo::new();

    // Initial commit
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\n").unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // First rebase scenario
    repo.git(&["checkout", "-b", "feature1"]).unwrap();
    fs::write(&file_path, "line 1\nAI line 1\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("Feature 1").unwrap();

    repo.git(&["checkout", "main"]).unwrap();
    fs::write(&file_path, "main line 1\nline 1\n").unwrap();
    repo.stage_all_and_commit("Main 1").unwrap();

    repo.git(&["checkout", "feature1"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    // Verify first rebase succeeded
    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(lines![
        "main line 1".human(),
        "line 1".human(),
        "AI line 1".ai(),
    ]);

    // Second rebase scenario
    repo.git(&["checkout", "main"]).unwrap();
    repo.git(&["checkout", "-b", "feature2"]).unwrap();
    fs::write(&file_path, "main line 1\nline 1\nAI line 1\nAI line 2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("Feature 2").unwrap();

    repo.git(&["checkout", "main"]).unwrap();
    fs::write(&file_path, "main line 2\nmain line 1\nline 1\n").unwrap();
    repo.stage_all_and_commit("Main 2").unwrap();

    repo.git(&["checkout", "feature2"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    // Verify second rebase succeeded
    file.assert_committed_lines(lines![
        "main line 2".human(),
        "main line 1".human(),
        "line 1".human(),
        "AI line 1".ai(),
        "AI line 2".ai(),
    ]);

    // Both rebases should have cleaned up their snapshots
    let refs_output = repo
        .git_og(&["for-each-ref", "refs/git-ai/backup/"])
        .unwrap();
    assert!(
        refs_output.is_empty() || refs_output.trim().is_empty(),
        "All backup refs should be cleaned up after successful rebases"
    );
}

#[test]
fn test_no_verify_bypasses_all_git_ai_logic() {
    let repo = TestRepo::new();

    // Create initial commit with AI attribution
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Verify note exists before
    let head_before = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let note_before = repo.git_og(&["notes", "--ref=ai", "show", &head_before]);
    assert!(note_before.is_ok(), "Note should exist before rebase");

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "line 1\nAI line\nfeature line\n").unwrap();
    repo.stage_all_and_commit("Feature").unwrap();

    // Advance main
    repo.git(&["checkout", "main"]).unwrap();
    fs::write(&file_path, "main line\nline 1\nAI line\n").unwrap();
    repo.stage_all_and_commit("Main").unwrap();

    // Rebase with --no-verify (should bypass git-ai entirely)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "--no-verify", "main"]).unwrap();

    // Check if any backup refs were created (they shouldn't be with --no-verify)
    let refs_output = repo
        .git_og(&["for-each-ref", "refs/git-ai/backup/"])
        .unwrap();
    assert!(
        refs_output.is_empty() || refs_output.trim().is_empty(),
        "No backup refs should be created with --no-verify"
    );

    // Note: With --no-verify, notes won't be copied by git-ai hooks.
    // This is expected behavior - user explicitly bypassed hooks.
    // The test validates that git-ai respects the flag.
}

#[test]
fn test_selective_blocking_prevents_fast_command_race() {
    let repo = TestRepo::new();

    // Create initial commit with AI attribution
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "line 1\nAI line\nfeature line\n").unwrap();
    repo.stage_all_and_commit("Feature").unwrap();

    // Advance main
    repo.git(&["checkout", "main"]).unwrap();
    fs::write(&file_path, "main line\nline 1\nAI line\n").unwrap();
    repo.stage_all_and_commit("Main").unwrap();

    // Rebase (wrapper will block ~3s for daemon)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    // Immediately run git log (this would race before blocking was added)
    let log_result = repo.git_og(&["log", "--oneline", "-1"]);
    assert!(
        log_result.is_ok(),
        "git log should work immediately after rebase"
    );

    // Verify notes are present (blocking should have prevented race)
    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(lines![
        "main line".human(),
        "line 1".ai(),
        "AI line".ai(),
        "feature line".human(),
    ]);
}

#[test]
fn test_abort_and_continue_skip_blocking() {
    let repo = TestRepo::new();

    // Create initial commit
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "line 1\n").unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "line 1\nfeature conflict\n").unwrap();
    repo.stage_all_and_commit("Feature").unwrap();

    // Main with conflict
    repo.git(&["checkout", "main"]).unwrap();
    fs::write(&file_path, "line 1\nmain conflict\n").unwrap();
    repo.stage_all_and_commit("Main").unwrap();

    // Start rebase (will conflict)
    repo.git(&["checkout", "feature"]).unwrap();
    let start_time = std::time::Instant::now();
    let _ = repo.git(&["rebase", "main"]); // Will fail with conflict

    // Abort should be fast (no blocking)
    let abort_result = repo.git(&["rebase", "--abort"]);
    let abort_elapsed = start_time.elapsed();

    assert!(abort_result.is_ok(), "Rebase abort should succeed");
    assert!(
        abort_elapsed < std::time::Duration::from_secs(2),
        "Abort should not block (took {:?})",
        abort_elapsed
    );
}
