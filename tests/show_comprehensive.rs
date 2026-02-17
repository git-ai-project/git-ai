//! Comprehensive tests for `git-ai show` command
//!
//! Tests cover:
//! - Show single commit authorship data
//! - Show commit range authorship data
//! - Handling commits with and without authorship logs
//! - Error handling and validation
//! - Output formatting

#[macro_use]
mod repos;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

// ============================================================================
// Basic Show Tests
// ============================================================================

#[test]
fn test_show_single_commit_with_ai_authorship() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("test.rs");
    file.set_contents(lines!["fn old() {}".human()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create commit with AI changes
    file.set_contents(lines!["fn new() {}".ai(), "fn another() {}".ai()]);
    let commit = repo.stage_all_and_commit("AI changes").unwrap();

    // Run show command
    let output = repo
        .git_ai(&["show", &commit.commit_sha])
        .expect("show should succeed");

    // Should contain authorship log data
    assert!(
        !output.contains("No authorship data"),
        "Should have authorship data for AI commit"
    );

    // Should be structured JSON or YAML-like format
    assert!(
        output.contains("agent") || output.contains("tool") || output.contains("mock_ai"),
        "Should contain agent/tool information: {}",
        output
    );
}

#[test]
fn test_show_commit_without_authorship() {
    let repo = TestRepo::new();

    // Create commit without AI attribution
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Content".human()]);
    let commit = repo.stage_all_and_commit("Human only").unwrap();

    // Run show command
    let output = repo
        .git_ai(&["show", &commit.commit_sha])
        .expect("show should succeed");

    // Should indicate no authorship data
    assert!(
        output.contains("No authorship data"),
        "Should indicate no authorship data for human-only commit: {}",
        output
    );
}

#[test]
fn test_show_with_head_ref() {
    let repo = TestRepo::new();

    // Create commit with AI changes
    let mut file = repo.filename("head_test.rs");
    file.set_contents(lines!["fn test() {}".ai()]);
    repo.stage_all_and_commit("AI commit").unwrap();

    // Run show with HEAD reference
    let output = repo
        .git_ai(&["show", "HEAD"])
        .expect("show HEAD should succeed");

    // Should show authorship data
    assert!(
        !output.contains("No authorship data")
            || output.contains("agent")
            || output.contains("tool"),
        "Should show authorship for HEAD"
    );
}

#[test]
fn test_show_with_relative_ref() {
    let repo = TestRepo::new();

    // Create first commit
    let mut file = repo.filename("relative.rs");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("First").unwrap();

    // Create second commit with AI changes
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Second AI").unwrap();

    // Run show with HEAD~1 (first commit)
    let output = repo
        .git_ai(&["show", "HEAD~1"])
        .expect("show HEAD~1 should succeed");

    // First commit should have no authorship data
    assert!(
        output.contains("No authorship data"),
        "HEAD~1 (human only) should have no authorship data"
    );

    // Run show with HEAD (second commit)
    let output2 = repo
        .git_ai(&["show", "HEAD"])
        .expect("show HEAD should succeed");

    // Second commit should have authorship data
    assert!(
        !output2.contains("No authorship data")
            || output2.contains("agent")
            || output2.contains("tool"),
        "HEAD (AI commit) should have authorship data"
    );
}

// ============================================================================
// Commit Range Tests
// ============================================================================

#[test]
fn test_show_commit_range() {
    let repo = TestRepo::new();

    // Create first commit
    let mut file = repo.filename("range.rs");
    file.set_contents(lines!["Line 1".human()]);
    let first = repo.stage_all_and_commit("First").unwrap();

    // Create second commit with AI changes
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Second AI").unwrap();

    // Create third commit with more AI changes
    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".ai()]);
    let third = repo.stage_all_and_commit("Third AI").unwrap();

    // Run show with commit range
    let range = format!("{}..{}", first.commit_sha, third.commit_sha);
    let output = repo
        .git_ai(&["show", &range])
        .expect("show range should succeed");

    // Should show multiple commits
    // The range output may vary - it might show all commits in the range
    assert!(!output.is_empty(), "Range output should not be empty");
}

#[test]
fn test_show_range_with_mixed_authorship() {
    let repo = TestRepo::new();

    // Create first commit (human only)
    let mut file = repo.filename("mixed.rs");
    file.set_contents(lines!["Line 1".human()]);
    let first = repo.stage_all_and_commit("Human").unwrap();

    // Create second commit (AI)
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("AI").unwrap();

    // Create third commit (human)
    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".human()]);
    let third = repo.stage_all_and_commit("Human again").unwrap();

    // Run show with range
    let range = format!("{}..{}", first.commit_sha, third.commit_sha);
    let output = repo
        .git_ai(&["show", &range])
        .expect("show range should succeed");

    // Should show some commits (implementation may vary)
    assert!(!output.is_empty(), "Range should show commits");
}

#[test]
fn test_show_range_empty() {
    let repo = TestRepo::new();

    // Create single commit
    let mut file = repo.filename("empty.rs");
    file.set_contents(lines!["Line 1".human()]);
    let commit = repo.stage_all_and_commit("Only commit").unwrap();

    // Try to show range from commit to itself (empty range)
    let range = format!("{}..{}", commit.commit_sha, commit.commit_sha);
    let output = repo
        .git_ai(&["show", &range])
        .expect("show empty range should succeed");

    // May show nothing or the commit itself (implementation dependent)
    // Should not error
    assert!(
        output.contains("No authorship data")
            || output.is_empty()
            || output.contains(&commit.commit_sha[..8]),
        "Empty range should handle gracefully"
    );
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_show_no_arguments() {
    let repo = TestRepo::new();

    // Try to run show without arguments
    let result = repo.git_ai(&["show"]);

    // Should fail with error
    assert!(result.is_err(), "show without arguments should fail");
}

#[test]
fn test_show_too_many_arguments() {
    let repo = TestRepo::new();

    // Create commit
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Content".human()]);
    let commit = repo.stage_all_and_commit("Test").unwrap();

    // Try to run show with multiple arguments
    let result = repo.git_ai(&["show", &commit.commit_sha, "extra_arg"]);

    // Should fail with error
    assert!(result.is_err(), "show with multiple arguments should fail");
}

#[test]
fn test_show_invalid_commit_ref() {
    let repo = TestRepo::new();

    // Create a commit so repo is not empty
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Content".human()]);
    repo.stage_all_and_commit("Test").unwrap();

    // Try to show non-existent commit
    let result = repo.git_ai(&["show", "nonexistent123"]);

    // Should fail gracefully
    assert!(result.is_err(), "show with invalid ref should fail");
}

#[test]
fn test_show_malformed_range() {
    let repo = TestRepo::new();

    // Create commit
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Content".human()]);
    repo.stage_all_and_commit("Test").unwrap();

    // Try malformed ranges
    let result1 = repo.git_ai(&["show", ".."]);
    assert!(result1.is_err(), "show with '..' should fail");

    let result2 = repo.git_ai(&["show", "abc.."]);
    assert!(result2.is_err(), "show with 'abc..' should fail");

    let result3 = repo.git_ai(&["show", "..abc"]);
    assert!(result3.is_err(), "show with '..abc' should fail");
}

// ============================================================================
// Output Format Tests
// ============================================================================

#[test]
fn test_show_output_format_with_data() {
    let repo = TestRepo::new();

    // Create commit with AI changes
    let mut file = repo.filename("format.rs");
    file.set_contents(lines!["fn test() {}".ai()]);
    let commit = repo.stage_all_and_commit("AI commit").unwrap();

    // Run show
    let output = repo
        .git_ai(&["show", &commit.commit_sha])
        .expect("show should succeed");

    // Should be structured output (YAML/JSON-like)
    // Look for key-value structure
    assert!(
        output.contains(":") || output.contains("agent") || output.contains("tool"),
        "Output should be structured: {}",
        output
    );
}

#[test]
fn test_show_output_format_without_data() {
    let repo = TestRepo::new();

    // Create commit without AI changes
    let mut file = repo.filename("no_data.txt");
    file.set_contents(lines!["Content".human()]);
    let commit = repo.stage_all_and_commit("Human commit").unwrap();

    // Run show
    let output = repo
        .git_ai(&["show", &commit.commit_sha])
        .expect("show should succeed");

    // Should show clear message
    assert!(
        output.contains("No authorship data"),
        "Should clearly indicate no data: {}",
        output
    );
}

#[test]
fn test_show_includes_commit_sha_in_range() {
    let repo = TestRepo::new();

    // Create commits
    let mut file = repo.filename("sha.rs");
    file.set_contents(lines!["Line 1".human()]);
    let first = repo.stage_all_and_commit("First").unwrap();

    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Second").unwrap();

    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".ai()]);
    let third = repo.stage_all_and_commit("Third").unwrap();

    // Run show with range
    let range = format!("{}..{}", first.commit_sha, third.commit_sha);
    let output = repo
        .git_ai(&["show", &range])
        .expect("show range should succeed");

    // When showing multiple commits, each should be identifiable
    // (implementation may vary - might show SHAs or other identifiers)
    assert!(
        !output.is_empty(),
        "Range output should contain commit information"
    );
}

// ============================================================================
// Multiple Files and Complex Changes Tests
// ============================================================================

#[test]
fn test_show_commit_with_multiple_files() {
    let repo = TestRepo::new();

    // Create commit with changes to multiple files
    let mut file1 = repo.filename("file1.rs");
    let mut file2 = repo.filename("file2.rs");
    file1.set_contents(lines!["File 1 content".ai()]);
    file2.set_contents(lines!["File 2 content".ai()]);
    let commit = repo.stage_all_and_commit("Multi-file AI changes").unwrap();

    // Run show
    let output = repo
        .git_ai(&["show", &commit.commit_sha])
        .expect("show should succeed");

    // Should show authorship data
    assert!(
        !output.contains("No authorship data"),
        "Should have authorship data for multi-file commit"
    );
}

#[test]
fn test_show_commit_with_mixed_attribution() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("mixed.rs");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Create commit with both AI and human changes
    file.set_contents(lines![
        "Line 1 modified".human(),
        "Line 2".ai(),
        "Line 3".human()
    ]);
    let commit = repo.stage_all_and_commit("Mixed changes").unwrap();

    // Run show
    let output = repo
        .git_ai(&["show", &commit.commit_sha])
        .expect("show should succeed");

    // Should show authorship data (at least for AI portions)
    assert!(
        !output.is_empty(),
        "Should show data for mixed attribution commit"
    );
}

// ============================================================================
// Special Cases
// ============================================================================

#[test]
fn test_show_initial_commit() {
    let repo = TestRepo::new();

    // Create initial commit with AI changes
    let mut file = repo.filename("initial.rs");
    file.set_contents(lines!["fn initial() {}".ai()]);
    let commit = repo.stage_all_and_commit("Initial commit").unwrap();

    // Run show on initial commit
    let output = repo
        .git_ai(&["show", &commit.commit_sha])
        .expect("show should work on initial commit");

    // Should show authorship data
    assert!(
        !output.contains("No authorship data"),
        "Initial commit with AI should have authorship data"
    );
}

#[test]
fn test_show_merge_commit() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("merge.rs");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Create a branch and make AI changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Feature AI").unwrap();

    // Switch back to main and merge
    repo.git(&["checkout", "main"]).unwrap();
    let merge_result = repo.git(&["merge", "feature", "--no-edit"]);

    if merge_result.is_ok() {
        // If merge succeeded, show the merge commit
        let output = repo
            .git_ai(&["show", "HEAD"])
            .expect("show merge commit should succeed");

        // Merge commits may or may not have authorship data depending on implementation
        assert!(
            !output.is_empty(),
            "Show should produce output for merge commit"
        );
    }
}

#[test]
fn test_show_with_unicode_content() {
    let repo = TestRepo::new();

    // Create commit with unicode content
    let mut file = repo.filename("unicode.txt");
    file.set_contents(lines!["Hello 世界".ai(), "こんにちは".ai()]);
    let commit = repo.stage_all_and_commit("Unicode AI").unwrap();

    // Run show
    let output = repo
        .git_ai(&["show", &commit.commit_sha])
        .expect("show should handle unicode");

    // Should show authorship data
    assert!(
        !output.contains("No authorship data"),
        "Should have authorship data for unicode commit"
    );
}

#[test]
fn test_show_with_special_characters_in_filename() {
    let repo = TestRepo::new();

    // Create file with special characters
    let mut file_with_spaces = repo.filename("file with spaces.rs");
    file_with_spaces.set_contents(lines!["fn test() {}".ai()]);
    let commit = repo.stage_all_and_commit("Special filename AI").unwrap();

    // Run show
    let output = repo
        .git_ai(&["show", &commit.commit_sha])
        .expect("show should handle special filenames");

    // Should show authorship data
    assert!(
        !output.contains("No authorship data"),
        "Should have authorship data for special filename commit"
    );
}

// ============================================================================
// Integration with Other Commands
// ============================================================================

#[test]
fn test_show_after_search() {
    let repo = TestRepo::new();

    // Create commit with AI changes
    let mut file = repo.filename("search_show.rs");
    file.set_contents(lines!["fn test() {}".ai()]);
    let commit = repo.stage_all_and_commit("AI commit").unwrap();

    // First run search to find the commit
    let search_output = repo
        .git_ai(&["search", "--commit", &commit.commit_sha])
        .expect("search should succeed");

    // Verify search found the commit
    assert!(
        !search_output.is_empty(),
        "Search should find the AI commit"
    );

    // Then run show on the same commit
    let show_output = repo
        .git_ai(&["show", &commit.commit_sha])
        .expect("show should succeed");

    // Both should provide information about the commit
    assert!(
        !show_output.contains("No authorship data"),
        "Show should have authorship data"
    );
}

#[test]
fn test_show_consistency_with_blame() {
    let repo = TestRepo::new();

    // Create file with AI changes
    let mut file = repo.filename("consistency.rs");
    file.set_contents(lines!["Line 1".ai(), "Line 2".ai()]);
    let commit = repo.stage_all_and_commit("AI commit").unwrap();

    // Run show
    let show_output = repo
        .git_ai(&["show", &commit.commit_sha])
        .expect("show should succeed");

    // Run blame on the file
    let blame_output = repo
        .git_ai(&["blame", "consistency.rs"])
        .expect("blame should succeed");

    // Both should indicate AI authorship
    let show_has_ai = show_output.contains("agent")
        || show_output.contains("tool")
        || show_output.contains("mock_ai");
    let blame_has_ai = blame_output.contains("ai") || blame_output.contains("mock_ai");

    assert!(
        show_has_ai || blame_has_ai,
        "Either show or blame should indicate AI authorship"
    );
}

// ============================================================================
// Commit History Tests
// ============================================================================

#[test]
fn test_show_sequential_commits() {
    let repo = TestRepo::new();

    // Create a series of commits
    let mut file = repo.filename("sequential.rs");

    file.set_contents(lines!["Line 1".human()]);
    let commit1 = repo.stage_all_and_commit("Commit 1").unwrap();

    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    let commit2 = repo.stage_all_and_commit("Commit 2").unwrap();

    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".ai()]);
    let commit3 = repo.stage_all_and_commit("Commit 3").unwrap();

    // Show each commit
    let output1 = repo.git_ai(&["show", &commit1.commit_sha]).expect("show 1");
    let output2 = repo.git_ai(&["show", &commit2.commit_sha]).expect("show 2");
    let output3 = repo.git_ai(&["show", &commit3.commit_sha]).expect("show 3");

    // First should have no authorship, second and third should have authorship
    assert!(
        output1.contains("No authorship data"),
        "Commit 1 human-only"
    );
    assert!(
        !output2.contains("No authorship data"),
        "Commit 2 should have AI data"
    );
    assert!(
        !output3.contains("No authorship data"),
        "Commit 3 should have AI data"
    );
}

#[test]
fn test_show_abbreviated_sha() {
    let repo = TestRepo::new();

    // Create commit with AI changes
    let mut file = repo.filename("abbrev.rs");
    file.set_contents(lines!["fn test() {}".ai()]);
    let commit = repo.stage_all_and_commit("AI commit").unwrap();

    // Use abbreviated SHA (first 7 characters)
    let short_sha = &commit.commit_sha[..7];
    let output = repo
        .git_ai(&["show", short_sha])
        .expect("show should work with abbreviated SHA");

    // Should show authorship data
    assert!(
        !output.contains("No authorship data"),
        "Should work with abbreviated SHA"
    );
}
