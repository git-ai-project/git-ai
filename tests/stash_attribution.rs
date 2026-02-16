mod repos;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

#[test]
fn test_stash_pop_with_ai_attribution() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create a file with AI attribution
    let mut example = repo.filename("example.txt");
    example.set_contents(vec!["line 1".ai(), "line 2".ai(), "line 3".ai()]);

    // Run checkpoint to track AI attribution
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash the changes
    repo.git(&["stash", "push", "-m", "test stash"])
        .expect("stash should succeed");

    // Verify file is gone
    assert!(repo.read_file("example.txt").is_none());

    // Pop the stash
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Verify file is back
    assert!(repo.read_file("example.txt").is_some());

    // Commit the changes
    let commit = repo
        .stage_all_and_commit("apply stashed changes")
        .expect("commit should succeed");

    // Verify AI attribution is preserved
    example.assert_lines_and_blame(vec!["line 1".ai(), "line 2".ai(), "line 3".ai()]);

    // Check authorship log has AI prompts
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
}

#[test]
fn test_stash_apply_with_ai_attribution() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create a file with AI attribution
    let mut example = repo.filename("example.txt");
    example.set_contents(vec!["line 1".ai(), "line 2".ai()]);

    // Run checkpoint to track AI attribution
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash the changes
    repo.git(&["stash"]).expect("stash should succeed");

    // Apply (not pop) the stash
    repo.git(&["stash", "apply"])
        .expect("stash apply should succeed");

    // Commit the changes
    let commit = repo
        .stage_all_and_commit("apply stashed changes")
        .expect("commit should succeed");

    // Verify AI attribution is preserved
    example.assert_lines_and_blame(vec!["line 1".ai(), "line 2".ai()]);

    // Check authorship log has AI prompts
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
}

#[test]
fn test_stash_apply_named_reference() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create first stash
    let mut file1 = repo.filename("file1.txt");
    file1.set_contents(vec!["first stash".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.git(&["stash"]).expect("first stash should succeed");

    // Create second stash
    let mut file2 = repo.filename("file2.txt");
    file2.set_contents(vec!["second stash".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.git(&["stash"]).expect("second stash should succeed");

    // Apply the first stash (stash@{1})
    repo.git(&["stash", "apply", "stash@{1}"])
        .expect("stash apply stash@{1} should succeed");

    // Verify file1 is back
    assert!(repo.read_file("file1.txt").is_some());
    assert!(repo.read_file("file2.txt").is_none());

    // Commit and verify attribution
    let commit = repo
        .stage_all_and_commit("apply first stash")
        .expect("commit should succeed");

    file1.assert_lines_and_blame(vec!["first stash".ai()]);

    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
}

#[test]
fn test_stash_pop_with_existing_stack_entries() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    let mut first = repo.filename("first.txt");
    first.set_contents(vec!["first stash line".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.git(&["stash", "push", "-m", "first"])
        .expect("first stash should succeed");

    let mut second = repo.filename("second.txt");
    second.set_contents(vec!["second stash line".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.git(&["stash", "push", "-m", "second"])
        .expect("second stash should succeed");

    // Pop when stash stack still has another entry (non-empty -> non-empty on some Git versions).
    repo.git(&["stash", "pop"])
        .expect("first pop should succeed");
    let first_pop_commit = repo
        .stage_all_and_commit("apply top stash entry")
        .expect("commit after first pop should succeed");

    second.assert_lines_and_blame(vec!["second stash line".ai()]);
    assert!(
        !first_pop_commit.authorship_log.metadata.prompts.is_empty(),
        "expected AI prompts for first pop commit"
    );

    // Pop remaining stash entry and verify attribution still restores correctly.
    repo.git(&["stash", "pop"])
        .expect("second pop should succeed");
    let second_pop_commit = repo
        .stage_all_and_commit("apply remaining stash entry")
        .expect("commit after second pop should succeed");

    first.assert_lines_and_blame(vec!["first stash line".ai()]);
    assert!(
        !second_pop_commit.authorship_log.metadata.prompts.is_empty(),
        "expected AI prompts for second pop commit"
    );
}

#[test]
fn test_stash_multiple_files() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create multiple files with AI attribution
    let mut file1 = repo.filename("file1.txt");
    file1.set_contents(vec!["file 1 line 1".ai(), "file 1 line 2".ai()]);

    let mut file2 = repo.filename("file2.txt");
    file2.set_contents(vec!["file 2 line 1".ai(), "file 2 line 2".ai()]);

    let mut file3 = repo.filename("file3.txt");
    file3.set_contents(vec!["file 3 line 1".ai()]);

    // Run checkpoint to track AI attribution
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash all changes
    repo.git(&["stash"]).expect("stash should succeed");

    // Verify files are gone
    assert!(repo.read_file("file1.txt").is_none());
    assert!(repo.read_file("file2.txt").is_none());
    assert!(repo.read_file("file3.txt").is_none());

    // Pop the stash
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Commit all files
    let commit = repo
        .stage_all_and_commit("apply multi-file stash")
        .expect("commit should succeed");

    // Verify all files have AI attribution
    file1.assert_lines_and_blame(vec!["file 1 line 1".ai(), "file 1 line 2".ai()]);
    file2.assert_lines_and_blame(vec!["file 2 line 1".ai(), "file 2 line 2".ai()]);
    file3.assert_lines_and_blame(vec!["file 3 line 1".ai()]);

    // Check authorship log has the files
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
    assert_eq!(
        commit.authorship_log.attestations.len(),
        3,
        "Expected 3 files in authorship log"
    );
}

#[test]
fn test_stash_with_existing_initial_attributions() {
    // Test that stash attributions merge correctly with existing INITIAL attributions
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create a file and commit it (this will have some attribution)
    let mut example = repo.filename("example.txt");
    example.set_contents(vec!["existing line".human()]);
    let _first_commit = repo
        .stage_all_and_commit("add example")
        .expect("commit should succeed");

    // Modify the file with AI
    example.set_contents(vec!["existing line".human(), "new AI line".ai()]);

    // Run checkpoint
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash the changes
    repo.git(&["stash"]).expect("stash should succeed");

    // Verify file reverted to original
    let content = repo.read_file("example.txt").expect("file should exist");
    assert_eq!(content.lines().count(), 1, "Should have reverted to 1 line");

    // Pop the stash
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Commit
    let commit = repo
        .stage_all_and_commit("apply stash")
        .expect("commit should succeed");

    // Verify mixed attribution
    example.assert_lines_and_blame(vec!["existing line".human(), "new AI line".ai()]);

    // Should have both human and AI in authorship
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
}

#[test]
fn test_stash_pop_default_reference() {
    // Test that stash pop defaults to stash@{0}
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create AI content
    let mut example = repo.filename("example.txt");
    example.set_contents(vec!["AI content".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash without explicit reference
    repo.git(&["stash"]).expect("stash should succeed");

    // Pop without explicit reference (should use stash@{0})
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Commit and verify
    let commit = repo
        .stage_all_and_commit("apply default stash")
        .expect("commit should succeed");

    example.assert_lines_and_blame(vec!["AI content".ai()]);

    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
}

#[test]
fn test_stash_pop_empty_repo() {
    // Test that stash operations don't crash on edge cases
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Try to pop when there's no stash - should fail gracefully
    let result = repo.git(&["stash", "pop"]);
    assert!(result.is_err(), "Should fail when no stash exists");
}

#[test]
fn test_stash_mixed_human_and_ai() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create file with mixed attribution
    let mut example = repo.filename("example.txt");
    example.set_contents(vec![
        "line 1".human(),
        "line 2".ai(),
        "line 3".human(),
        "line 4".ai(),
    ]);

    // Run checkpoint
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash and pop
    repo.git(&["stash"]).expect("stash should succeed");
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Commit
    let commit = repo
        .stage_all_and_commit("mixed content")
        .expect("commit should succeed");

    // Verify blame shows mixed attribution
    example.assert_lines_and_blame(vec![
        "line 1".human(),
        "line 2".ai(),
        "line 3".human(),
        "line 4".ai(),
    ]);

    // Authorship log should have AI prompts
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
}

#[test]
fn test_stash_push_with_pathspec_single_file() {
    // Test git stash push -- file.txt only stashes that file
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create two files with AI content
    let mut file1 = repo.filename("file1.txt");
    file1.set_contents(vec!["file1 line 1".ai(), "file1 line 2".ai()]);

    let mut file2 = repo.filename("file2.txt");
    file2.set_contents(vec!["file2 line 1".ai(), "file2 line 2".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash only file1.txt
    repo.git(&["stash", "push", "--", "file1.txt"])
        .expect("stash push should succeed");

    // file1 should be gone, file2 should still exist
    assert!(repo.read_file("file1.txt").is_none());
    assert!(repo.read_file("file2.txt").is_some());

    // Pop the stash
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Now file1 is back
    assert!(repo.read_file("file1.txt").is_some());

    // Commit everything
    let commit = repo
        .stage_all_and_commit("apply partial stash")
        .expect("commit should succeed");

    // Both files should have AI attribution
    file1.assert_lines_and_blame(vec!["file1 line 1".ai(), "file1 line 2".ai()]);
    file2.assert_lines_and_blame(vec!["file2 line 1".ai(), "file2 line 2".ai()]);

    // Should have AI prompts
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
}

#[test]
fn test_stash_push_with_pathspec_directory() {
    // Test git stash push -- dir/ only stashes that directory
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create files in a directory and root
    let mut root_file = repo.filename("root.txt");
    root_file.set_contents(vec!["root line 1".ai()]);

    // Create src directory
    std::fs::create_dir_all(repo.path().join("src")).expect("Failed to create src dir");

    let mut dir_file1 = repo.filename("src/file1.txt");
    dir_file1.set_contents(vec!["src file1 line 1".ai()]);

    let mut dir_file2 = repo.filename("src/file2.txt");
    dir_file2.set_contents(vec!["src file2 line 1".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash only src/ directory
    repo.git(&["stash", "push", "--", "src/"])
        .expect("stash push should succeed");

    // src files should be gone, root file should remain
    assert!(repo.read_file("src/file1.txt").is_none());
    assert!(repo.read_file("src/file2.txt").is_none());
    assert!(repo.read_file("root.txt").is_some());

    // Pop the stash
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Commit everything
    let commit = repo
        .stage_all_and_commit("apply directory stash")
        .expect("commit should succeed");

    // All files should have AI attribution
    root_file.assert_lines_and_blame(vec!["root line 1".ai()]);
    dir_file1.assert_lines_and_blame(vec!["src file1 line 1".ai()]);
    dir_file2.assert_lines_and_blame(vec!["src file2 line 1".ai()]);

    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
}

#[test]
fn test_stash_push_multiple_pathspecs() {
    // Test git stash push -- file1.txt file2.txt
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create three files with AI content
    let mut file1 = repo.filename("file1.txt");
    file1.set_contents(vec!["file1".ai()]);

    let mut file2 = repo.filename("file2.txt");
    file2.set_contents(vec!["file2".ai()]);

    let mut file3 = repo.filename("file3.txt");
    file3.set_contents(vec!["file3".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash only file1 and file2
    repo.git(&["stash", "push", "--", "file1.txt", "file2.txt"])
        .expect("stash push should succeed");

    // file1 and file2 should be gone, file3 remains
    assert!(repo.read_file("file1.txt").is_none());
    assert!(repo.read_file("file2.txt").is_none());
    assert!(repo.read_file("file3.txt").is_some());

    // Pop the stash
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Commit everything
    let commit = repo
        .stage_all_and_commit("apply multi-pathspec stash")
        .expect("commit should succeed");

    // All files should have AI attribution
    file1.assert_lines_and_blame(vec!["file1".ai()]);
    file2.assert_lines_and_blame(vec!["file2".ai()]);
    file3.assert_lines_and_blame(vec!["file3".ai()]);

    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
}

#[test]
fn test_stash_pop_with_conflict() {
    // Test that attribution is preserved when there's a conflict during stash pop
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create a file with mixed human and AI content
    let mut example = repo.filename("example.txt");
    example.set_contents(vec![
        "header".human(),
        "line 1 AI".ai(),
        "line 2 AI".ai(),
        "footer".human(),
    ]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash the changes
    repo.git(&["stash"]).expect("stash should succeed");

    // Now create a conflicting version with different mixed content
    example.set_contents(vec![
        "header".human(),
        "line 1 different".ai(),
        "line 2 different".ai(),
        "footer".human(),
    ]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.stage_all_and_commit("conflicting changes")
        .expect("commit should succeed");

    // Try to pop - this WILL create a conflict
    let _result = repo.git(&["stash", "pop"]);

    // Verify there's a conflict
    let content = repo.read_file("example.txt").expect("file should exist");
    assert!(
        content.contains("<<<<<<<"),
        "Expected conflict markers in file, got: {}",
        content
    );

    // Manually resolve the conflict by taking parts from both versions
    example.set_contents(vec![
        "header".human(),        // from both (same)
        "line 1 AI".ai(),        // from stash
        "line 2 different".ai(), // from HEAD
        "footer".human(),        // from both (same)
    ]);

    // Mark as resolved and commit
    repo.git(&["add", "example.txt"])
        .expect("should be able to add resolved file");

    let _commit = repo
        .stage_all_and_commit("resolved conflict")
        .expect("commit should succeed");

    // Verify mixed human and AI attributions are preserved
    example.assert_lines_and_blame(vec![
        "header".human(),
        "line 1 AI".ai(),
        "line 2 different".ai(),
        "footer".human(),
    ]);
}

#[test]
fn test_stash_mixed_staged_and_unstaged() {
    // Test stashing with a mix of staged and unstaged changes
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create a file with AI content
    let mut example = repo.filename("example.txt");
    example.set_contents(vec!["staged line 1".ai(), "staged line 2".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stage these changes
    repo.git(&["add", "example.txt"])
        .expect("should stage example.txt");

    // Now add more unstaged changes
    example.set_contents(vec![
        "staged line 1".ai(),
        "staged line 2".ai(),
        "unstaged line 3".ai(),
        "unstaged line 4".ai(),
    ]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash both staged and unstaged (git stash by default stashes both)
    repo.git(&["stash", "--include-untracked"])
        .expect("stash should succeed");

    // Verify file is back to original state (doesn't exist)
    assert!(repo.read_file("example.txt").is_none());

    // Pop the stash
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Commit all changes
    let commit = repo
        .stage_all_and_commit("apply mixed stash")
        .expect("commit should succeed");

    // All lines should have AI attribution preserved (both staged and unstaged)
    example.assert_lines_and_blame(vec![
        "staged line 1".ai(),
        "staged line 2".ai(),
        "unstaged line 3".ai(),
        "unstaged line 4".ai(),
    ]);

    // Should have AI prompts
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
}

#[test]
fn test_stash_pop_onto_head_with_ai_changes() {
    // Test that popping stash onto a HEAD with AI changes preserves both attributions
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create file1 with AI content from first session
    let mut file1 = repo.filename("file1.txt");
    file1.set_contents(vec!["file1 line 1".ai(), "file1 line 2".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash file1
    repo.git(&["stash"]).expect("stash should succeed");
    assert!(repo.read_file("file1.txt").is_none());

    // Now create file2 with AI content and commit it to HEAD
    let mut file2 = repo.filename("file2.txt");
    file2.set_contents(vec![
        "file2 line 1".ai(),
        "file2 line 2".ai(),
        "file2 line 3".ai(),
    ]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    let head_commit = repo
        .stage_all_and_commit("add file2 with AI")
        .expect("commit should succeed");

    // Verify HEAD has AI attribution
    file2.assert_lines_and_blame(vec![
        "file2 line 1".ai(),
        "file2 line 2".ai(),
        "file2 line 3".ai(),
    ]);
    assert!(
        !head_commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in HEAD commit"
    );

    // Pop the stash (file1 with AI attribution from stash)
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Commit the popped changes
    let final_commit = repo
        .stage_all_and_commit("apply stash onto HEAD with AI")
        .expect("commit should succeed");

    // Verify BOTH files maintain their AI attributions:
    // file1 should have AI attribution from the stash
    file1.assert_lines_and_blame(vec!["file1 line 1".ai(), "file1 line 2".ai()]);

    // file2 should STILL have AI attribution (unchanged from HEAD)
    file2.assert_lines_and_blame(vec![
        "file2 line 1".ai(),
        "file2 line 2".ai(),
        "file2 line 3".ai(),
    ]);

    // The authorship log should track file1 (the new changes from stash)
    // file2 should already be in the repo from the previous commit
    assert!(
        final_commit
            .authorship_log
            .attestations
            .iter()
            .any(|a| a.file_path.ends_with("file1.txt")),
        "Expected file1.txt in authorship log"
    );
}

#[test]
fn test_stash_pop_across_branches() {
    // Test that AI attributions are preserved when stashing, switching branches, and popping
    let repo = TestRepo::new();

    // Create initial commit on main branch
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create a file with existing human content
    let mut example = repo.filename("example.txt");
    example.set_contents(vec!["line 1".human(), "line 2".human(), "line 3".human()]);
    repo.stage_all_and_commit("add example file")
        .expect("commit should succeed");

    // Add 5 AI-generated lines at the bottom
    example.set_contents(vec![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "AI line 1".ai(),
        "AI line 2".ai(),
        "AI line 3".ai(),
        "AI line 4".ai(),
        "AI line 5".ai(),
    ]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash the AI changes
    repo.git(&["stash"]).expect("stash should succeed");

    // Verify file reverted to 3 lines
    let content = repo.read_file("example.txt").expect("file should exist");
    assert_eq!(
        content.lines().count(),
        3,
        "Should have reverted to 3 lines"
    );

    // Create and checkout a new branch
    repo.git(&["checkout", "-b", "feature-branch"])
        .expect("should create and checkout new branch");

    // Pop the stash on the new branch
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Commit the changes on the new branch
    let commit = repo
        .stage_all_and_commit("apply AI changes on feature branch")
        .expect("commit should succeed");

    // Verify all AI attributions are preserved
    example.assert_lines_and_blame(vec![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "AI line 1".ai(),
        "AI line 2".ai(),
        "AI line 3".ai(),
        "AI line 4".ai(),
        "AI line 5".ai(),
    ]);

    // Should have AI prompts in authorship log
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
}

#[test]
fn test_stash_pop_across_branches_with_conflict() {
    // Test that AI attributions are preserved when resolving conflicts after stash pop across branches
    let repo = TestRepo::new();

    // Create initial commit on main branch
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create a file with existing content
    let mut example = repo.filename("example.txt");
    example.set_contents(vec!["line 1".human(), "line 2".human(), "line 3".human()]);
    repo.stage_all_and_commit("add example file")
        .expect("commit should succeed");

    // Add 5 AI-generated lines at the bottom
    example.set_contents(vec![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "AI line 1".ai(),
        "AI line 2".ai(),
        "AI line 3".ai(),
        "AI line 4".ai(),
        "AI line 5".ai(),
    ]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash the AI changes
    repo.git(&["stash"]).expect("stash should succeed");

    // Create and checkout a new branch
    repo.git(&["checkout", "-b", "feature-branch"])
        .expect("should create and checkout new branch");

    // Make conflicting changes on the new branch (add different content at the bottom)
    example.set_contents(vec![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "feature line 1".ai(),
        "feature line 2".ai(),
    ]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.stage_all_and_commit("add feature content")
        .expect("commit should succeed");

    // Try to pop the stash - this will create a conflict
    let _result = repo.git(&["stash", "pop"]);

    // Verify there's a conflict
    let content = repo.read_file("example.txt").expect("file should exist");
    assert!(
        content.contains("<<<<<<<") || content.contains(">>>>>>>"),
        "Expected conflict markers in file"
    );

    // Resolve the conflict by keeping both (feature branch lines + stashed AI lines)
    example.set_contents(vec![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "feature line 1".ai(),
        "feature line 2".ai(),
        "AI line 1".ai(),
        "AI line 2".ai(),
        "AI line 3".ai(),
        "AI line 4".ai(),
        "AI line 5".ai(),
    ]);

    // Mark as resolved and commit
    repo.git(&["add", "example.txt"])
        .expect("should be able to add resolved file");

    let commit = repo
        .stage_all_and_commit("resolved conflict keeping both changes")
        .expect("commit should succeed");

    // Verify all AI attributions are preserved for both sets of changes
    example.assert_lines_and_blame(vec![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "feature line 1".ai(),
        "feature line 2".ai(),
        "AI line 1".ai(),
        "AI line 2".ai(),
        "AI line 3".ai(),
        "AI line 4".ai(),
        "AI line 5".ai(),
    ]);

    // Should have AI prompts in authorship log
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log"
    );
}

#[test]
fn test_stash_apply_reset_apply_again() {
    // Test that AI attributions survive multiple apply/reset cycles
    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create a file with AI content
    let mut example = repo.filename("example.txt");
    example.set_contents(vec!["AI line 1".ai(), "AI line 2".ai(), "AI line 3".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash the changes (using regular stash, not apply, so we can test the workflow)
    repo.git(&["stash"]).expect("stash should succeed");
    assert!(repo.read_file("example.txt").is_none());

    // Apply the stash (NOT pop, so it stays in the stash list)
    repo.git(&["stash", "apply", "stash@{0}"])
        .expect("stash apply should succeed");
    assert!(repo.read_file("example.txt").is_some());

    // Reset to undo the apply
    repo.git(&["reset", "--hard"])
        .expect("reset should succeed");
    assert!(repo.read_file("example.txt").is_none());

    // Apply the same stash again
    repo.git(&["stash", "apply", "stash@{0}"])
        .expect("second stash apply should succeed");
    assert!(repo.read_file("example.txt").is_some());

    // Commit the changes
    let commit = repo
        .stage_all_and_commit("apply stash after reset")
        .expect("commit should succeed");

    // Verify AI attribution is preserved after multiple apply/reset cycles
    example.assert_lines_and_blame(vec!["AI line 1".ai(), "AI line 2".ai(), "AI line 3".ai()]);

    // Check authorship log has AI prompts
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log after multiple apply/reset cycles"
    );
}
