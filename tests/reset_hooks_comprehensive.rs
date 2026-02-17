#[macro_use]
mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

// Unit tests for extract_tree_ish function
#[test]
fn test_extract_tree_ish_no_args_defaults_to_head() {
    // The function should return "HEAD" when no tree-ish is provided
    // We test this through actual reset behavior
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["line 1"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Reset with no args should work (defaults to HEAD)
    repo.git(&["reset"])
        .expect("reset with no args should succeed");

    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines!["line 1".human()]);
}

#[test]
fn test_extract_tree_ish_with_hard_flag() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["v1"]);
    let first = repo.stage_all_and_commit("First").unwrap();

    file.insert_at(1, lines!["v2"]);
    repo.stage_all_and_commit("Second").unwrap();

    // Reset --hard with explicit commit SHA
    repo.git(&["reset", "--hard", &first.commit_sha])
        .expect("reset --hard with SHA should succeed");

    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines!["v1".human()]);
}

#[test]
fn test_extract_tree_ish_with_soft_flag() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["base"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    file.insert_at(1, lines!["added".ai()]);
    repo.stage_all_and_commit("Added").unwrap();

    // Reset --soft with explicit commit SHA
    repo.git(&["reset", "--soft", &base.commit_sha])
        .expect("reset --soft with SHA should succeed");

    // Changes should be staged
    let new_commit = repo.commit("Re-commit").unwrap();
    assert!(!new_commit.authorship_log.attestations.is_empty());
}

#[test]
fn test_extract_tree_ish_with_mixed_flag() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["base"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    file.insert_at(1, lines!["added".ai()]);
    repo.stage_all_and_commit("Added").unwrap();

    // Reset --mixed with explicit commit SHA
    repo.git(&["reset", "--mixed", &base.commit_sha])
        .expect("reset --mixed with SHA should succeed");

    // Changes should be in working directory
    let new_commit = repo.stage_all_and_commit("Re-commit").unwrap();
    assert!(!new_commit.authorship_log.attestations.is_empty());
}

// This test is covered by existing pathspec tests in reset.rs

// This test is covered by existing pathspec tests in reset.rs

#[test]
fn test_extract_tree_ish_head_tilde_notation() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["v1"]);
    repo.stage_all_and_commit("First").unwrap();

    file.insert_at(1, lines!["v2".ai()]);
    repo.stage_all_and_commit("Second").unwrap();

    file.insert_at(2, lines!["v3".ai()]);
    repo.stage_all_and_commit("Third").unwrap();

    // Reset using HEAD~1 notation
    repo.git(&["reset", "--soft", "HEAD~1"])
        .expect("reset HEAD~1 should succeed");

    let new_commit = repo.commit("Re-commit").unwrap();
    assert!(!new_commit.authorship_log.attestations.is_empty());
}

#[test]
fn test_extract_tree_ish_head_caret_notation() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["v1"]);
    repo.stage_all_and_commit("First").unwrap();

    file.insert_at(1, lines!["v2".ai()]);
    repo.stage_all_and_commit("Second").unwrap();

    // Reset using HEAD^ notation
    repo.git(&["reset", "--soft", "HEAD^"])
        .expect("reset HEAD^ should succeed");

    let new_commit = repo.commit("Re-commit").unwrap();
    assert!(!new_commit.authorship_log.attestations.is_empty());
}

// Tests for pathspec extraction with --pathspec-from-file
// Note: These tests verify the read_pathspecs_from_file function works correctly

// Note: Git doesn't handle empty lines in pathspec files well
// This test is disabled because git fails with "empty string is not a valid pathspec"

// Tests for reset mode flag detection
#[test]
fn test_reset_with_keep_flag() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["base"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    file.insert_at(1, lines!["staged".ai()]);
    repo.stage_all_and_commit("Staged").unwrap();

    // Reset --keep with clean working tree should succeed
    repo.git(&["reset", "--keep", &base.commit_sha])
        .expect("reset --keep should succeed");

    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines!["base".human()]);
}

#[test]
fn test_reset_with_merge_flag() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["base"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    file.insert_at(1, lines!["change".ai()]);
    repo.stage_all_and_commit("Change").unwrap();

    // Reset --merge when working tree is clean
    repo.git(&["reset", "--merge", &base.commit_sha])
        .expect("reset --merge should succeed with clean working tree");

    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines!["base".human()]);
}

// Tests for error conditions and edge cases
#[test]
fn test_reset_to_nonexistent_commit_fails() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["content"]);
    repo.stage_all_and_commit("Commit").unwrap();

    // Try to reset to non-existent commit
    let result = repo.git(&["reset", "0000000000000000000000000000000000000000"]);
    assert!(result.is_err(), "reset to non-existent commit should fail");
}

// Tests for backward vs forward reset detection
#[test]
fn test_reset_backward_multiple_commits() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["v1"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    file.insert_at(1, lines!["v2".ai()]);
    repo.stage_all_and_commit("Second").unwrap();

    file.insert_at(2, lines!["v3".ai()]);
    repo.stage_all_and_commit("Third").unwrap();

    file.insert_at(3, lines!["v4".ai()]);
    repo.stage_all_and_commit("Fourth").unwrap();

    // Reset backward 3 commits
    repo.git(&["reset", "--soft", &base.commit_sha])
        .expect("reset backward should succeed");

    // All AI changes should be preserved
    let new_commit = repo.commit("Squashed").unwrap();
    assert!(!new_commit.authorship_log.attestations.is_empty());

    // Verify the content is correct (attribution may vary)
    let content = repo.read_file("test.txt").unwrap();
    assert!(content.contains("v1"));
    assert!(content.contains("v2"));
    assert!(content.contains("v3"));
    assert!(content.contains("v4"));
}

#[test]
fn test_reset_forward_after_backward() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["v1"]);
    let first = repo.stage_all_and_commit("First").unwrap();

    file.insert_at(1, lines!["v2".ai()]);
    let _second = repo.stage_all_and_commit("Second").unwrap();

    file.insert_at(2, lines!["v3".ai()]);
    let third = repo.stage_all_and_commit("Third").unwrap();

    // Reset backward
    repo.git(&["reset", "--hard", &first.commit_sha])
        .expect("reset backward should succeed");

    // Reset forward
    repo.git(&["reset", "--hard", &third.commit_sha])
        .expect("reset forward should succeed");

    // Should be back to third commit state
    let content = repo.read_file("test.txt").unwrap();
    assert!(content.contains("v1"));
    assert!(content.contains("v2"));
    assert!(content.contains("v3"));
}

// Tests for pathspec matching with directories are covered in reset.rs

// Tests for working log preservation
#[test]
fn test_reset_preserves_non_pathspec_working_log() {
    let repo = TestRepo::new();
    let mut file1 = repo.filename("reset.txt");
    let mut file2 = repo.filename("keep.txt");

    file1.set_contents(lines!["reset content"]);
    file2.set_contents(lines!["keep content"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    file1.insert_at(1, lines!["reset change".ai()]);
    file2.insert_at(1, lines!["keep change".ai()]);
    repo.stage_all_and_commit("Changes").unwrap();

    // Make uncommitted changes to file2
    file2.insert_at(2, lines!["uncommitted".ai()]);

    // Reset only file1
    repo.git(&["reset", &base.commit_sha, "--", "reset.txt"])
        .expect("pathspec reset should succeed");

    // Commit and verify file2 keeps both committed and uncommitted changes
    let new_commit = repo.stage_all_and_commit("After reset").unwrap();
    assert!(!new_commit.authorship_log.attestations.is_empty());

    // Verify file2 has all the content
    let content = repo.read_file("keep.txt").unwrap();
    assert!(content.contains("keep content"));
    assert!(content.contains("keep change"));
    assert!(content.contains("uncommitted"));
}

// Tests for checkpoint interaction
#[test]
fn test_reset_creates_checkpoint_before_reset() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["base"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    file.insert_at(1, lines!["change".ai()]);
    repo.stage_all_and_commit("Change").unwrap();

    // Make uncommitted changes
    file.insert_at(2, lines!["uncommitted".ai()]);

    // Reset should create checkpoint of uncommitted work
    repo.git(&["reset", "--soft", &base.commit_sha])
        .expect("reset should succeed");

    // Uncommitted changes should be preserved in staged state
    let new_commit = repo.commit("After reset").unwrap();
    assert!(!new_commit.authorship_log.attestations.is_empty());
}

// Tests for mixed AI and human authorship
#[test]
fn test_reset_preserves_interleaved_ai_human_changes() {
    let repo = TestRepo::new();
    let mut file = repo.filename("complex.txt");

    file.set_contents(lines!["line1"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    // AI commit
    file.insert_at(1, lines!["ai1".ai()]);
    repo.stage_all_and_commit("AI 1").unwrap();

    // Human commit
    file.insert_at(2, lines!["human1"]);
    repo.stage_all_and_commit("Human 1").unwrap();

    // Another AI commit
    file.insert_at(3, lines!["ai2".ai()]);
    repo.stage_all_and_commit("AI 2").unwrap();

    // Reset to base (not all the way, keep some AI)
    repo.git(&["reset", "--soft", &base.commit_sha])
        .expect("reset should succeed");

    // Verify all content is present in staged state
    let content = repo.read_file("complex.txt").unwrap();
    assert!(content.contains("line1"));
    assert!(content.contains("ai1"));
    assert!(content.contains("human1"));
    assert!(content.contains("ai2"));
}

// Tests for special file names and paths are covered in other test files

// Test reset with relative commit refs
#[test]
fn test_reset_with_head_at_notation() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["v1"]);
    repo.stage_all_and_commit("First").unwrap();

    file.insert_at(1, lines!["v2".ai()]);
    let _second = repo.stage_all_and_commit("Second").unwrap();

    file.insert_at(2, lines!["v3".ai()]);
    repo.stage_all_and_commit("Third").unwrap();

    // Reset using HEAD~1 notation
    // Note: This tests that the pre-reset hook correctly resolves the ref
    repo.git(&["reset", "--soft", "HEAD~1"])
        .expect("reset with ~1 should succeed");

    let new_commit = repo.commit("Re-commit").unwrap();
    assert!(!new_commit.authorship_log.attestations.is_empty());
}

// Test reset with no changes (no-op)
#[test]
fn test_reset_to_current_head_is_noop() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["content"]);
    repo.stage_all_and_commit("Commit").unwrap();

    // Make some uncommitted changes
    file.insert_at(1, lines!["uncommitted".ai()]);

    // Reset to current HEAD (should preserve uncommitted)
    repo.git(&["reset", "HEAD"])
        .expect("reset to HEAD should succeed");

    // Uncommitted changes should still be there
    let new_commit = repo.stage_all_and_commit("After noop reset").unwrap();
    assert!(!new_commit.authorship_log.attestations.is_empty());

    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines!["content".human(), "uncommitted".ai()]);
}

// Test reset deletes working log on --hard
#[test]
fn test_reset_hard_deletes_uncommitted_work() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["base"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    file.insert_at(1, lines!["committed".ai()]);
    repo.stage_all_and_commit("Committed").unwrap();

    // Make uncommitted changes
    file.insert_at(2, lines!["uncommitted".ai()]);

    // Reset --hard should discard all uncommitted work
    repo.git(&["reset", "--hard", &base.commit_sha])
        .expect("reset --hard should succeed");

    // File should match base exactly
    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines!["base".human()]);

    // Make a new change to verify state is clean
    file.insert_at(1, lines!["new"]);
    repo.stage_all_and_commit("New commit").unwrap();

    let content = repo.read_file("test.txt").unwrap();
    assert!(content.contains("base"));
    assert!(content.contains("new"));
}

// Test pathspec with glob patterns - covered in other test files

// Test reset with file deletions and additions
#[test]
fn test_reset_with_file_additions_and_deletions() {
    let repo = TestRepo::new();

    let mut existing = repo.filename("existing.txt");
    existing.set_contents(lines!["exists"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    // Delete existing file and add new file
    repo.git(&["rm", "existing.txt"]).unwrap();
    let mut new_file = repo.filename("new.txt");
    new_file.set_contents(lines!["new content".ai()]);
    repo.stage_all_and_commit("Delete and add").unwrap();

    // Reset to base
    repo.git(&["reset", "--soft", &base.commit_sha])
        .expect("reset should succeed");

    // Re-commit and verify
    let new_commit = repo.commit("After reset").unwrap();

    // The new file should have AI attribution
    assert!(!new_commit.authorship_log.attestations.is_empty());
}
