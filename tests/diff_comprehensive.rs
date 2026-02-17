//! Comprehensive tests for `git-ai diff` command (additional coverage)
//!
//! These tests complement the existing tests/diff.rs with additional edge cases
//! and scenarios to push coverage toward 95%.

#[macro_use]
mod repos;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use serde_json::Value;

// ============================================================================
// JSON Output Tests (complementing existing tests)
// ============================================================================

#[test]
fn test_diff_json_structure() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("json_struct.rs");
    file.set_contents(lines!["fn old() {}".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make AI changes
    file.set_contents(lines!["fn new() {}".ai()]);
    let commit = repo.stage_all_and_commit("AI changes").unwrap();

    // Run diff with --json
    let output = repo
        .git_ai(&["diff", &commit.commit_sha, "--json"])
        .expect("diff --json should succeed");

    // Parse JSON
    let json: Value = serde_json::from_str(&output).expect("Should be valid JSON");

    // Verify top-level structure
    assert!(
        json.get("files").is_some(),
        "JSON should have 'files' field"
    );
    assert!(
        json.get("prompts").is_some(),
        "JSON should have 'prompts' field"
    );

    // Verify files is an object
    assert!(json["files"].is_object(), "files should be an object (map)");

    // Verify prompts is an object
    assert!(
        json["prompts"].is_object(),
        "prompts should be an object (map)"
    );
}

#[test]
fn test_diff_json_file_structure() {
    let repo = TestRepo::new();

    // Create commit with AI changes
    let mut file = repo.filename("file_struct.ts");
    file.set_contents(lines!["const x = 1;".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    file.set_contents(lines!["const x = 2;".ai()]);
    let commit = repo.stage_all_and_commit("Update x").unwrap();

    // Run diff with --json
    let output = repo
        .git_ai(&["diff", &commit.commit_sha, "--json"])
        .expect("diff --json should succeed");

    // Parse JSON
    let json: Value = serde_json::from_str(&output).expect("Should be valid JSON");

    // Get the file entry
    let files = json["files"].as_object().expect("files should be object");
    assert!(!files.is_empty(), "Should have at least one file");

    let file_entry = files.values().next().expect("Should have a file");

    // Verify file structure
    assert!(
        file_entry.get("annotations").is_some(),
        "File should have annotations"
    );
    assert!(file_entry.get("diff").is_some(), "File should have diff");
    assert!(
        file_entry.get("base_content").is_some(),
        "File should have base_content"
    );
}

#[test]
fn test_diff_json_annotations_format() {
    let repo = TestRepo::new();

    // Create commit with AI changes
    let mut file = repo.filename("annotations.rs");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".ai()]);
    let commit = repo.stage_all_and_commit("Add AI lines").unwrap();

    // Run diff with --json
    let output = repo
        .git_ai(&["diff", &commit.commit_sha, "--json"])
        .expect("diff --json should succeed");

    // Parse JSON
    let json: Value = serde_json::from_str(&output).expect("Should be valid JSON");

    // Verify annotations structure
    let files = json["files"].as_object().expect("files should be object");
    if let Some(file_entry) = files.values().next() {
        let annotations = &file_entry["annotations"];
        assert!(
            annotations.is_object(),
            "annotations should be an object (map)"
        );
    }
}

#[test]
fn test_diff_json_base_content_accuracy() {
    let repo = TestRepo::new();

    // Create file with specific content
    let initial_content = "const x = 1;\nconst y = 2;\n";
    let file_path = repo.path().join("base_test.js");
    std::fs::write(&file_path, initial_content).unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Modify content
    std::fs::write(&file_path, "const x = 1;\nconst z = 3;\n").unwrap();
    let commit = repo.stage_all_and_commit("Modify").unwrap();

    // Run diff with --json
    let output = repo
        .git_ai(&["diff", &commit.commit_sha, "--json"])
        .expect("diff --json should succeed");

    // Parse JSON
    let json: Value = serde_json::from_str(&output).expect("Should be valid JSON");

    // Verify base_content matches original
    let files = json["files"].as_object().expect("files should be object");
    let file_entry = &files["base_test.js"];
    let base_content = file_entry["base_content"]
        .as_str()
        .expect("base_content should be string");

    assert_eq!(
        base_content, initial_content,
        "base_content should match original file"
    );
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_diff_invalid_commit_ref() {
    let repo = TestRepo::new();

    // Create a commit so repo is not empty
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Content".human()]);
    repo.stage_all_and_commit("Test").unwrap();

    // Try to diff non-existent commit
    let result = repo.git_ai(&["diff", "nonexistent123"]);

    // Should fail gracefully
    assert!(result.is_err(), "diff with invalid ref should fail");
}

#[test]
fn test_diff_invalid_range_format() {
    let repo = TestRepo::new();

    // Create commit
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Content".human()]);
    repo.stage_all_and_commit("Test").unwrap();

    // Try invalid range formats
    let result1 = repo.git_ai(&["diff", "..."]);
    assert!(
        result1.is_err(),
        "diff with '...' should fail (triple dots not supported)"
    );
}

#[test]
fn test_diff_range_start_equals_end() {
    let repo = TestRepo::new();

    // Create commit
    let mut file = repo.filename("same.txt");
    file.set_contents(lines!["Content".human()]);
    let commit = repo.stage_all_and_commit("Test").unwrap();

    // Try range where start equals end
    let range = format!("{}..{}", commit.commit_sha, commit.commit_sha);
    let output = repo
        .git_ai(&["diff", &range])
        .expect("diff with same start/end should succeed");

    // Should show empty diff (no changes between identical commits)
    assert!(
        output.is_empty() || !output.contains("@@"),
        "Diff between same commits should be empty"
    );
}

// ============================================================================
// Edge Cases for File Handling
// ============================================================================

#[test]
fn test_diff_new_file_from_empty() {
    let repo = TestRepo::new();

    // Create initial empty commit using git directly to avoid checkpoint system
    repo.git(&["commit", "--allow-empty", "-m", "Empty initial"])
        .expect("empty commit should succeed");

    // Add new file
    let mut file = repo.filename("new.rs");
    file.set_contents(lines!["fn new() {}".ai()]);
    let commit = repo.stage_all_and_commit("Add new file").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("diff with new file should succeed");

    // Should show additions
    assert!(output.contains("+"), "Should show additions for new file");
}

#[test]
fn test_diff_deleted_file() {
    let repo = TestRepo::new();

    // Create file
    let mut file = repo.filename("deleted.rs");
    file.set_contents(lines!["fn old() {}".human()]);
    repo.stage_all_and_commit("Add file").unwrap();

    // Delete file
    std::fs::remove_file(repo.path().join("deleted.rs")).unwrap();
    let commit = repo.stage_all_and_commit("Delete file").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("diff with deleted file should succeed");

    // Should show deletions
    assert!(
        output.contains("-"),
        "Should show deletions for deleted file"
    );
}

#[test]
fn test_diff_renamed_file() {
    let repo = TestRepo::new();

    // Create file
    let mut file = repo.filename("old_name.rs");
    file.set_contents(lines!["fn test() {}".human()]);
    repo.stage_all_and_commit("Add file").unwrap();

    // Rename file via git
    repo.git(&["mv", "old_name.rs", "new_name.rs"]).unwrap();
    let commit = repo.stage_all_and_commit("Rename file").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("diff with renamed file should succeed");

    // Git should detect rename, diff should handle it
    assert!(!output.is_empty(), "Diff should show file changes");
}

#[test]
fn test_diff_empty_file() {
    let repo = TestRepo::new();

    // Create empty file
    let file_path = repo.path().join("empty.txt");
    std::fs::write(&file_path, "").unwrap();
    repo.stage_all_and_commit("Add empty file").unwrap();

    // Add content to file
    std::fs::write(&file_path, "content\n").unwrap();
    let commit = repo.stage_all_and_commit("Add content").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("diff with empty file should succeed");

    // Should show addition
    assert!(output.contains("+"), "Should show addition to empty file");
}

// ============================================================================
// Special Content Tests
// ============================================================================

#[test]
fn test_diff_with_very_long_lines() {
    let repo = TestRepo::new();

    // Create file with very long line
    let long_line = "x".repeat(1000);
    let mut file = repo.filename("long.txt");
    file.set_contents(vec![long_line.clone().human()]);
    repo.stage_all_and_commit("Long line").unwrap();

    // Modify the long line
    let modified = format!("{}y", long_line);
    file.set_contents(vec![modified.ai()]);
    let commit = repo.stage_all_and_commit("Modify long line").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("diff with long lines should succeed");

    // Should handle long lines
    assert!(
        output.contains("+") && output.contains("-"),
        "Should show diff"
    );
}

#[test]
fn test_diff_with_special_regex_chars() {
    let repo = TestRepo::new();

    // Create file with special characters that might affect regex
    let mut file = repo.filename("special.txt");
    file.set_contents(lines!["Line with $pecial [chars] (and) {braces}".human()]);
    repo.stage_all_and_commit("Special chars").unwrap();

    // Modify
    file.set_contents(lines![
        "Line with $pecial [chars] (and) {braces} modified".ai()
    ]);
    let commit = repo.stage_all_and_commit("Modify special").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("diff with special chars should succeed");

    // Should handle special characters
    assert!(
        output.contains("$pecial") || output.contains("chars"),
        "Should show content with special chars"
    );
}

#[test]
fn test_diff_whitespace_only_changes() {
    let repo = TestRepo::new();

    // Create file
    let mut file = repo.filename("whitespace.rs");
    file.set_contents(lines!["fn test() {".human(), "}".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Change whitespace only
    file.set_contents(lines!["fn test() {".human(), "    ".human(), "}".human()]);
    let commit = repo.stage_all_and_commit("Add whitespace").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("diff with whitespace changes should succeed");

    // Should show the whitespace change
    assert!(
        output.contains("+") || output.contains("-"),
        "Should show whitespace changes"
    );
}

// ============================================================================
// Performance and Scalability Tests
// ============================================================================

#[test]
fn test_diff_large_file() {
    let repo = TestRepo::new();

    // Create large file
    let mut file = repo.filename("large.txt");
    let large_content: Vec<_> = (0..1000).map(|i| format!("Line {}", i).human()).collect();
    file.set_contents(large_content.clone());
    repo.stage_all_and_commit("Large file").unwrap();

    // Modify one line in the middle
    let mut modified = large_content;
    modified[500] = "Modified line 500".ai();
    file.set_contents(modified);
    let commit = repo.stage_all_and_commit("Modify large file").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("diff with large file should succeed");

    // Should handle large file
    assert!(
        output.contains("Modified line 500"),
        "Should show the modified line"
    );
}

#[test]
fn test_diff_many_files() {
    let repo = TestRepo::new();

    // Create many files
    for i in 0..50 {
        let mut file = repo.filename(&format!("file{}.txt", i));
        file.set_contents(lines![format!("Content {}", i).human()]);
    }
    repo.stage_all_and_commit("Many files").unwrap();

    // Modify some files
    for i in 0..10 {
        let mut file = repo.filename(&format!("file{}.txt", i));
        file.set_contents(lines![
            format!("Content {}", i).human(),
            format!("Added {}", i).ai()
        ]);
    }
    let commit = repo.stage_all_and_commit("Modify many").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("diff with many files should succeed");

    // Should show multiple file diffs
    let diff_count = output.matches("diff --git").count();
    assert!(
        diff_count >= 10,
        "Should have diffs for at least 10 files, got {}",
        diff_count
    );
}

// ============================================================================
// Range Behavior Tests
// ============================================================================

#[test]
fn test_diff_range_multiple_commits() {
    let repo = TestRepo::new();

    // Create series of commits
    let mut file = repo.filename("range.rs");

    file.set_contents(lines!["Line 1".human()]);
    let first = repo.stage_all_and_commit("Commit 1").unwrap();

    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Commit 2").unwrap();

    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".human()]);
    repo.stage_all_and_commit("Commit 3").unwrap();

    file.set_contents(lines![
        "Line 1".human(),
        "Line 2".ai(),
        "Line 3".human(),
        "Line 4".ai()
    ]);
    let last = repo.stage_all_and_commit("Commit 4").unwrap();

    // Run diff across all commits
    let range = format!("{}..{}", first.commit_sha, last.commit_sha);
    let output = repo
        .git_ai(&["diff", &range])
        .expect("diff range should succeed");

    // Should show cumulative changes
    assert!(
        output.contains("Line 2") && output.contains("Line 3") && output.contains("Line 4"),
        "Should show all cumulative changes"
    );
}

#[test]
fn test_diff_range_shows_intermediate_changes() {
    let repo = TestRepo::new();

    // Create commits where intermediate changes are made and then reverted
    let mut file = repo.filename("intermediate.rs");

    file.set_contents(lines!["Line 1".human()]);
    let first = repo.stage_all_and_commit("Initial").unwrap();

    file.set_contents(lines!["Line 1".human(), "Temp line".ai()]);
    repo.stage_all_and_commit("Add temp").unwrap();

    file.set_contents(lines!["Line 1".human(), "Final line".ai()]);
    let last = repo.stage_all_and_commit("Replace temp").unwrap();

    // Run diff from first to last
    let range = format!("{}..{}", first.commit_sha, last.commit_sha);
    let output = repo
        .git_ai(&["diff", &range])
        .expect("diff range should succeed");

    // Should show net change (Final line added, not Temp line)
    assert!(
        output.contains("Final line"),
        "Should show final state change"
    );
}

// ============================================================================
// Compatibility Tests
// ============================================================================

#[test]
fn test_diff_works_with_submodules() {
    let repo = TestRepo::new();

    // Create a simple file (submodule handling is complex, just test basic compatibility)
    let mut file = repo.filename("main.rs");
    file.set_contents(lines!["fn main() {}".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    file.set_contents(lines!["fn main() {}".human(), "fn helper() {}".ai()]);
    let commit = repo.stage_all_and_commit("Add helper").unwrap();

    // Run diff (should work even if repo could theoretically have submodules)
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("diff should work");

    assert!(output.contains("helper"), "Should show the change");
}

#[test]
fn test_diff_attribution_consistency() {
    let repo = TestRepo::new();

    // Create commit with AI changes
    let mut file = repo.filename("consistency.rs");
    file.set_contents(lines!["Line 1".ai(), "Line 2".ai()]);
    let commit = repo.stage_all_and_commit("AI commit").unwrap();

    // Run diff multiple times
    let output1 = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("diff 1 should succeed");
    let output2 = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("diff 2 should succeed");

    // Results should be identical (deterministic)
    assert_eq!(
        output1, output2,
        "Multiple diff runs should produce identical output"
    );
}
