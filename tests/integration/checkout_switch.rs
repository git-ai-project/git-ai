use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{TestRepo, default_branchname};

/// Test that checkout to a different branch migrates the working log to the new HEAD.
#[test]
fn test_checkout_branch_migrates_working_log() {
    let repo = TestRepo::new();

    // Create initial commit on main
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Create a feature branch
    repo.git(&["branch", "feature"])
        .expect("branch creation should succeed");

    // Create AI changes (uncommitted)
    let mut ai_file = repo.filename("ai_work.txt");
    ai_file.set_contents(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Checkout feature branch
    repo.git(&["checkout", "feature"])
        .expect("checkout should succeed");

    // Commit and verify AI attribution is preserved
    repo.stage_all_and_commit("commit on feature branch")
        .expect("commit should succeed");

    ai_file.assert_lines_and_blame(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);
}

/// Test that force checkout (-f) deletes the working log when changes are discarded.
#[test]
fn test_checkout_force_deletes_working_log() {
    let repo = TestRepo::new();

    // Create initial commit on main
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Create second commit
    let mut file2 = repo.filename("file2.txt");
    file2.set_contents(vec!["Some content".to_string()]);
    repo.stage_all_and_commit("second commit")
        .expect("second commit should succeed");

    // Create AI changes (uncommitted)
    let mut ai_file = repo.filename("ai_work.txt");
    ai_file.set_contents(vec!["AI generated line".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Force checkout to previous commit (discards uncommitted changes)
    repo.git(&["checkout", "-f", "HEAD~1"])
        .expect("checkout -f should succeed");

    // The AI file should be gone (changes were discarded)
    assert!(
        repo.read_file("ai_work.txt").is_none(),
        "ai_work.txt should not exist after force checkout"
    );
}

/// Test that pathspec checkout removes attributions only for the specified files.
#[test]
fn test_checkout_pathspec_removes_file_attributions() {
    let repo = TestRepo::new();

    // Create initial commit with a file
    let mut original = repo.filename("original.txt");
    original.set_contents(vec!["Original content".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Create AI changes in multiple files
    let mut ai_file1 = repo.filename("ai_work1.txt");
    ai_file1.set_contents(vec!["AI line in file 1".ai()]);

    let mut ai_file2 = repo.filename("ai_work2.txt");
    ai_file2.set_contents(vec!["AI line in file 2".ai()]);

    // Also modify the original file with AI
    original.set_contents(vec!["Modified by AI".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Pathspec checkout - revert only original.txt to HEAD version
    repo.git(&["checkout", "HEAD", "--", "original.txt"])
        .expect("pathspec checkout should succeed");

    // Commit and verify:
    // - ai_file1 and ai_file2 still have AI attribution
    // - original.txt reverted to "Original content" (human)
    repo.stage_all_and_commit("commit after pathspec checkout")
        .expect("commit should succeed");

    ai_file1.assert_lines_and_blame(vec!["AI line in file 1".ai()]);
    ai_file2.assert_lines_and_blame(vec!["AI line in file 2".ai()]);
    original.assert_lines_and_blame(vec!["Original content".human()]);
}

/// Test that git switch migrates the working log to the new HEAD.
#[test]
fn test_switch_branch_migrates_working_log() {
    let repo = TestRepo::new();

    // Create initial commit on main
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Create a feature branch
    repo.git(&["branch", "feature"])
        .expect("branch creation should succeed");

    // Create AI changes (uncommitted)
    let mut ai_file = repo.filename("ai_work.txt");
    ai_file.set_contents(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Switch to feature branch
    repo.git(&["switch", "feature"])
        .expect("switch should succeed");

    // Commit and verify AI attribution is preserved
    repo.stage_all_and_commit("commit on feature branch")
        .expect("commit should succeed");

    ai_file.assert_lines_and_blame(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);
}

/// Test that switch --discard-changes deletes the working log.
#[test]
fn test_switch_discard_changes_deletes_working_log() {
    let repo = TestRepo::new();

    // Create initial commit on main
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Create a feature branch and switch to it, then back to main
    repo.git(&["branch", "feature"])
        .expect("branch creation should succeed");
    repo.git(&["switch", "feature"])
        .expect("switch to feature should succeed");

    // Make a commit on feature
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(vec!["Feature content".to_string()]);
    repo.stage_all_and_commit("feature commit")
        .expect("feature commit should succeed");

    // Switch back to main
    repo.git(&["switch", default_branchname()])
        .expect("switch to main should succeed");

    // Create AI changes on main (uncommitted)
    let mut ai_file = repo.filename("ai_work.txt");
    ai_file.set_contents(vec!["AI generated line".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Force switch to feature, discarding changes
    repo.git(&["switch", "--discard-changes", "feature"])
        .expect("switch --discard-changes should succeed");

    // The AI file should be gone (changes were discarded)
    assert!(
        repo.read_file("ai_work.txt").is_none(),
        "ai_work.txt should not exist after switch --discard-changes"
    );
}

/// Test that switch -f (short form of --discard-changes) deletes the working log.
#[test]
fn test_switch_force_flag_deletes_working_log() {
    let repo = TestRepo::new();

    // Create initial commit on main
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Create a feature branch with a commit
    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout -b should succeed");
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(vec!["Feature content".to_string()]);
    repo.stage_all_and_commit("feature commit")
        .expect("feature commit should succeed");

    // Switch back to main
    repo.git(&["switch", default_branchname()])
        .expect("switch to main should succeed");

    // Create AI changes on main (uncommitted)
    let mut ai_file = repo.filename("ai_work.txt");
    ai_file.set_contents(vec!["AI generated line".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Force switch using -f flag
    repo.git(&["switch", "-f", "feature"])
        .expect("switch -f should succeed");

    // The AI file should be gone (changes were discarded)
    assert!(
        repo.read_file("ai_work.txt").is_none(),
        "ai_work.txt should not exist after switch -f"
    );
}

/// Test that checkout with --merge migrates the working log when merging changes.
#[test]
fn test_checkout_merge_migrates_working_log() {
    let repo = TestRepo::new();

    // Create initial commit on main
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Create a feature branch
    repo.git(&["branch", "feature"])
        .expect("branch creation should succeed");

    // Create AI changes in a new file (unstaged, just written to disk)
    let mut ai_file = repo.filename("ai_work.txt");
    ai_file.set_contents(vec![
        "Human wrote this line".human(),
        "AI generated this code".ai(),
    ]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Unstage the file so --merge can work (use original git to avoid reset hook)
    repo.git_og(&["reset", "HEAD", "ai_work.txt"])
        .expect("unstage should succeed");

    // Checkout with --merge to feature branch
    repo.git(&["checkout", "--merge", "feature"])
        .expect("checkout --merge should succeed");

    // Commit and verify mixed attribution is preserved
    repo.stage_all_and_commit("commit on feature branch")
        .expect("commit should succeed");

    ai_file.assert_lines_and_blame(vec![
        "Human wrote this line".human(),
        "AI generated this code".ai(),
    ]);
}

/// Test that switch with --merge migrates the working log when merging changes.
#[test]
fn test_switch_merge_migrates_working_log() {
    let repo = TestRepo::new();

    // Create initial commit on main
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Create a feature branch
    repo.git(&["branch", "feature"])
        .expect("branch creation should succeed");

    // Create AI changes in a new file (unstaged, just written to disk)
    let mut ai_file = repo.filename("ai_work.txt");
    ai_file.set_contents(vec![
        "Human wrote this line".human(),
        "AI generated this code".ai(),
    ]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Unstage the file so --merge can work (use original git to avoid reset hook)
    repo.git_og(&["reset", "HEAD", "ai_work.txt"])
        .expect("unstage should succeed");

    // Switch with --merge to feature branch
    repo.git(&["switch", "--merge", "feature"])
        .expect("switch --merge should succeed");

    // Commit and verify mixed attribution is preserved
    repo.stage_all_and_commit("commit on feature branch")
        .expect("commit should succeed");

    ai_file.assert_lines_and_blame(vec![
        "Human wrote this line".human(),
        "AI generated this code".ai(),
    ]);
}

/// Test that checkout to the same branch is a no-op for working log.
#[test]
fn test_checkout_same_branch_no_op() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Create AI changes (uncommitted)
    let mut ai_file = repo.filename("ai_work.txt");
    ai_file.set_contents(vec!["AI generated line".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Checkout same branch (should be no-op)
    repo.git(&["checkout", default_branchname()])
        .expect("checkout same branch should succeed");

    // Commit and verify AI attribution is preserved
    repo.stage_all_and_commit("commit after same branch checkout")
        .expect("commit should succeed");

    ai_file.assert_lines_and_blame(vec!["AI generated line".ai()]);
}

/// Test with mixed human and AI attribution during checkout.
#[test]
fn test_checkout_with_mixed_attribution() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Create mixed human and AI changes
    let mut mixed_file = repo.filename("mixed.txt");
    mixed_file.set_contents(vec![
        "Human line 1".human(),
        "AI generated line".ai(),
        "Human line 2".human(),
    ]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Create and checkout new branch
    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout -b should succeed");

    // Commit and verify mixed attribution is preserved
    repo.stage_all_and_commit("commit with mixed attribution")
        .expect("commit should succeed");

    mixed_file.assert_lines_and_blame(vec![
        "Human line 1".human(),
        "AI generated line".ai(),
        "Human line 2".human(),
    ]);
}

/// Test pathspec checkout removes attributions for multiple files.
#[test]
fn test_checkout_pathspec_multiple_files() {
    let repo = TestRepo::new();

    // Create initial commit with multiple files
    let mut file_a = repo.filename("file_a.txt");
    file_a.set_contents(vec!["Original A".to_string()]);
    let mut file_b = repo.filename("file_b.txt");
    file_b.set_contents(vec!["Original B".to_string()]);
    let mut file_c = repo.filename("file_c.txt");
    file_c.set_contents(vec!["Original C".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Modify all files with AI
    file_a.set_contents(vec!["Modified A by AI".ai()]);
    file_b.set_contents(vec!["Modified B by AI".ai()]);
    file_c.set_contents(vec!["Modified C by AI".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Pathspec checkout - revert file_a and file_b, keep file_c
    repo.git(&["checkout", "HEAD", "--", "file_a.txt", "file_b.txt"])
        .expect("pathspec checkout should succeed");

    // Commit and verify:
    // - file_a and file_b reverted to original (human)
    // - file_c still has AI attribution
    repo.stage_all_and_commit("commit after pathspec checkout")
        .expect("commit should succeed");

    file_a.assert_lines_and_blame(vec!["Original A".human()]);
    file_b.assert_lines_and_blame(vec!["Original B".human()]);
    file_c.assert_lines_and_blame(vec!["Modified C by AI".ai()]);
}

/// Regression test for #957: `checkout --merge` that produces conflict markers in the
/// working tree must not corrupt AI attribution for lines that came from an AI session.
///
/// Bug: `restore_stashed_va` reads working-tree files to merge VA snapshots, but
/// when a file contains conflict markers the byte-level diff algorithm mismapped
/// attribution onto the wrong content.  The fix strips conflict markers (keeping
/// "ours") before the VA merge so byte offsets are computed on clean content.
#[test]
fn test_checkout_merge_conflict_preserves_ai_attribution() {
    let repo = TestRepo::new();

    // Initial commit: single line that both branches will modify, guaranteeing a
    // conflict when we later do `checkout --merge`.
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["shared"]);
    repo.stage_all_and_commit("initial").unwrap();
    let main_branch = repo.current_branch();

    // Feature branch: replace "shared" with "THEIRS".
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.replace_at(0, "THEIRS");
    repo.stage_all_and_commit("feature: change shared").unwrap();

    // Back on main: AI replaces "shared" with "AI_CONTENT" (different from "THEIRS"),
    // leaving the change in the working tree (not yet committed), then checkpoint.
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut main_file = repo.filename("file.txt");
    main_file.replace_at(0, "AI_CONTENT".ai());
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    // `checkout --merge feature`: base="shared", ours (working)="AI_CONTENT",
    // theirs (feature)="THEIRS" — all three differ → git produces conflict markers.
    let checkout_result = repo.git(&["checkout", "--merge", "feature"]);

    // Depending on the git version and config the exit code may be 0 or 1 even with
    // conflict markers present.  Handle both: if it failed we know there are markers;
    // if it succeeded git resolved without markers (uncommon but possible).
    let file_on_disk = std::fs::read_to_string(repo.path().join("file.txt"))
        .expect("file.txt must exist after checkout");

    if file_on_disk.contains("<<<<<<<") {
        // Conflict markers are present — this is the path that exercises the fix.
        // Resolve by keeping the AI content and discarding "THEIRS".
        std::fs::write(repo.path().join("file.txt"), "AI_CONTENT\n")
            .expect("write resolved content");
        repo.git(&["add", "file.txt"]).unwrap();
        repo.stage_all_and_commit("resolved: keep AI_CONTENT")
            .unwrap();

        // After fix: restore_stashed_va correctly stripped conflict markers before
        // computing the VA merge, so "AI_CONTENT" retains AI attribution.
        file.assert_lines_and_blame(crate::lines!["AI_CONTENT".ai()]);
    } else {
        // git resolved without markers; if the AI content survived, it should be AI.
        if file_on_disk.trim() == "AI_CONTENT" {
            let _ = repo.stage_all_and_commit("after clean checkout");
            file.assert_lines_and_blame(crate::lines!["AI_CONTENT".ai()]);
        }
        // If git took "THEIRS" without asking, there is nothing AI to assert.
    }
    let _ = checkout_result;
}

crate::reuse_tests_in_worktree!(
    test_checkout_branch_migrates_working_log,
    test_checkout_force_deletes_working_log,
    test_checkout_pathspec_removes_file_attributions,
    test_switch_branch_migrates_working_log,
    test_switch_discard_changes_deletes_working_log,
    test_switch_force_flag_deletes_working_log,
    test_checkout_merge_migrates_working_log,
    test_switch_merge_migrates_working_log,
    test_checkout_same_branch_no_op,
    test_checkout_with_mixed_attribution,
    test_checkout_pathspec_multiple_files,
    test_checkout_merge_conflict_preserves_ai_attribution,
);
