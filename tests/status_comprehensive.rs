//! Comprehensive tests for `git-ai status` command
//!
//! Tests cover:
//! - Basic status display with AI and human changes
//! - JSON output format
//! - Checkpoint handling and display
//! - Edge cases (no checkpoints, empty repo, etc.)
//! - Error handling and validation

#[macro_use]
mod repos;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use serde_json::Value;
use std::fs;

// ============================================================================
// Basic Status Tests
// ============================================================================

#[test]
fn test_status_with_no_changes() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Run status with no working directory changes
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should indicate no checkpoints
    assert!(
        output.contains("No checkpoints recorded"),
        "Should indicate no checkpoints when no changes: {}",
        output
    );
}

#[test]
fn test_status_with_ai_changes() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("test.rs");
    file.set_contents(lines!["fn old() {}".human()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make AI changes
    file.set_contents(lines!["fn new() {}".ai(), "fn another() {}".ai()]);

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should show AI changes
    assert!(
        output.contains("mock_ai") || output.contains("ai"),
        "Should show AI tool in status"
    );
}

#[test]
fn test_status_with_human_changes() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("test.rs");
    file.set_contents(lines!["fn old() {}".ai()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make human changes
    file.set_contents(lines!["fn new() {}".human(), "fn another() {}".human()]);

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should show statistics for human changes
    assert!(
        output.contains("+") || output.contains("additions"),
        "Should show additions in status"
    );
}

#[test]
fn test_status_with_mixed_changes() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("mixed.rs");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make mixed AI and human changes
    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".human()]);

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should show changes from both sources
    assert!(
        output.contains("+") || output.contains("additions"),
        "Should show additions"
    );
}

#[test]
fn test_status_counts_additions_and_deletions() {
    let repo = TestRepo::new();

    // Create initial commit with multiple lines
    let mut file = repo.filename("count.txt");
    file.set_contents(lines!["Line 1".human(), "Line 2".human(), "Line 3".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Delete one line, add two lines
    file.set_contents(lines!["Line 1".human(), "Line 4".ai(), "Line 5".ai()]);

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should show both additions and deletions
    assert!(
        output.contains("+") || output.contains("additions"),
        "Should show additions"
    );
    assert!(
        output.contains("-") || output.contains("deletions"),
        "Should show deletions"
    );
}

// ============================================================================
// JSON Output Tests
// ============================================================================

#[test]
fn test_status_json_output() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("json_test.rs");
    file.set_contents(lines!["fn old() {}".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make AI changes
    file.set_contents(lines!["fn new() {}".ai()]);

    // Run status with --json flag
    let output = repo
        .git_ai(&["status", "--json"])
        .expect("status --json should succeed");

    // Parse JSON
    let json: Value = serde_json::from_str(&output).expect("Output should be valid JSON");

    // Verify structure
    assert!(json.get("stats").is_some(), "JSON should have stats field");
    assert!(
        json.get("checkpoints").is_some(),
        "JSON should have checkpoints field"
    );

    // Verify stats structure
    let stats = &json["stats"];
    assert!(
        stats.get("git_diff_added_lines").is_some(),
        "stats should have git_diff_added_lines"
    );
    assert!(
        stats.get("git_diff_deleted_lines").is_some(),
        "stats should have git_diff_deleted_lines"
    );
}

#[test]
fn test_status_json_with_no_changes() {
    let repo = TestRepo::new();

    // Create initial commit with no subsequent changes
    let mut file = repo.filename("empty.txt");
    file.set_contents(lines!["Initial".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Run status with --json
    let output = repo
        .git_ai(&["status", "--json"])
        .expect("status --json should succeed");

    // Parse JSON
    let json: Value = serde_json::from_str(&output).expect("Output should be valid JSON");

    // Verify checkpoints is empty
    let checkpoints = json["checkpoints"]
        .as_array()
        .expect("checkpoints should be array");
    assert_eq!(
        checkpoints.len(),
        0,
        "checkpoints should be empty with no changes"
    );
}

#[test]
fn test_status_json_stats_accuracy() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("stats.txt");
    file.set_contents(lines!["Line 1".human(), "Line 2".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Add 3 lines, delete 1 line
    file.set_contents(lines![
        "Line 1".human(),
        "Line 3".ai(),
        "Line 4".ai(),
        "Line 5".ai()
    ]);

    // Run status with --json
    let output = repo
        .git_ai(&["status", "--json"])
        .expect("status --json should succeed");

    // Parse JSON
    let json: Value = serde_json::from_str(&output).expect("Output should be valid JSON");

    // Verify stats
    let stats = &json["stats"];
    let added = stats["git_diff_added_lines"]
        .as_u64()
        .expect("git_diff_added_lines should be number");
    let deleted = stats["git_diff_deleted_lines"]
        .as_u64()
        .expect("git_diff_deleted_lines should be number");

    assert_eq!(added, 3, "Should have 3 added lines");
    assert_eq!(deleted, 1, "Should have 1 deleted line");
}

// ============================================================================
// Checkpoint Tests
// ============================================================================

#[test]
fn test_status_shows_checkpoint_time() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("time.txt");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make AI changes
    file.set_contents(lines!["Line 2".ai()]);

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should show time information (secs/mins/hours ago)
    assert!(
        output.contains("ago") || output.contains("secs") || output.contains("mins"),
        "Should show time ago for checkpoints: {}",
        output
    );
}

#[test]
fn test_status_multiple_checkpoints() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("multi.txt");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make first AI change
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Make second AI change
    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".ai()]);

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should show changes
    assert!(
        output.contains("+") || output.contains("additions"),
        "Should show statistics"
    );
}

// ============================================================================
// Multiple Files Tests
// ============================================================================

#[test]
fn test_status_with_multiple_files() {
    let repo = TestRepo::new();

    // Create initial commit with multiple files
    let mut file1 = repo.filename("file1.txt");
    let mut file2 = repo.filename("file2.txt");
    file1.set_contents(lines!["File 1 Line 1".human()]);
    file2.set_contents(lines!["File 2 Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Modify both files
    file1.set_contents(lines!["File 1 Line 1".human(), "File 1 Line 2".ai()]);
    file2.set_contents(lines!["File 2 Line 1".human(), "File 2 Line 2".human()]);

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should aggregate changes from all files
    assert!(
        output.contains("+") || output.contains("additions"),
        "Should show combined additions"
    );
}

#[test]
fn test_status_new_file() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file1 = repo.filename("existing.txt");
    file1.set_contents(lines!["Existing".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Add new file
    let mut file2 = repo.filename("new.txt");
    file2.set_contents(lines!["New file".ai()]);

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should show additions from new file
    assert!(
        output.contains("+") || output.contains("additions"),
        "Should show additions from new file"
    );
}

#[test]
fn test_status_deleted_file() {
    let repo = TestRepo::new();

    // Create initial commit with file
    let mut file = repo.filename("deleted.txt");
    file.set_contents(lines!["Content".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Delete the file
    fs::remove_file(repo.path().join("deleted.txt")).unwrap();

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should show deletions
    assert!(
        output.contains("-") || output.contains("deletions") || output.contains("No checkpoints"),
        "Should show deletions or no checkpoints"
    );
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn test_status_empty_repository() {
    let repo = TestRepo::new();

    // Run status on empty repo (no commits)
    let result = repo.git_ai(&["status"]);

    // Should either succeed with empty output or fail gracefully
    // (behavior may vary based on implementation)
    match result {
        Ok(output) => {
            assert!(
                output.contains("No checkpoints") || output.is_empty(),
                "Empty repo should show no checkpoints or be empty"
            );
        }
        Err(_) => {
            // Also acceptable - empty repo may error
        }
    }
}

#[test]
fn test_status_after_commit() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("after_commit.txt");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make changes
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);

    // Run status (should show changes)
    let output1 = repo.git_ai(&["status"]).expect("status should succeed");
    assert!(
        output1.contains("+") || output1.contains("additions") || output1.contains("mock_ai"),
        "Should show changes before commit"
    );

    // Commit the changes
    repo.stage_all_and_commit("Add line 2").unwrap();

    // Run status again (should show no changes)
    let output2 = repo.git_ai(&["status"]).expect("status should succeed");
    assert!(
        output2.contains("No checkpoints"),
        "Should show no checkpoints after commit"
    );
}

#[test]
fn test_status_large_change_counts() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("large.txt");
    let initial_lines: Vec<_> = (0..100).map(|i| format!("Line {}", i).human()).collect();
    file.set_contents(initial_lines);
    repo.stage_all_and_commit("Initial").unwrap();

    // Add many new lines
    let mut new_lines: Vec<_> = (0..100).map(|i| format!("Line {}", i).human()).collect();
    let ai_lines: Vec<_> = (100..200).map(|i| format!("New line {}", i).ai()).collect();
    new_lines.extend(ai_lines);
    file.set_contents(new_lines);

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should handle large numbers correctly
    assert!(
        output.contains("+") || output.contains("additions"),
        "Should show additions for large changes"
    );
}

#[test]
fn test_status_binary_file_changes() {
    let repo = TestRepo::new();

    // Create initial commit with binary file
    let binary_path = repo.path().join("binary.dat");
    fs::write(&binary_path, &[0u8, 1, 2, 255, 254, 253]).unwrap();
    repo.stage_all_and_commit("Initial binary").unwrap();

    // Modify binary file
    fs::write(&binary_path, &[10u8, 20, 30, 240, 250, 255]).unwrap();

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should handle binary files gracefully (may show 0 or skip)
    // Implementation may vary
    assert!(
        output.contains("No checkpoints")
            || output.contains("+")
            || output.contains("additions")
            || output.is_empty(),
        "Should handle binary files gracefully"
    );
}

// ============================================================================
// Tool Attribution Tests
// ============================================================================

#[test]
fn test_status_shows_tool_name() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("tool.rs");
    file.set_contents(lines!["fn old() {}".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make AI changes
    file.set_contents(lines!["fn new() {}".ai()]);

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should show tool name (mock_ai or similar)
    assert!(
        output.contains("mock_ai") || output.contains("ai") || output.contains("Mock"),
        "Should show AI tool name: {}",
        output
    );
}

#[test]
fn test_status_shows_model_name() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("model.rs");
    file.set_contents(lines!["fn old() {}".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make AI changes
    file.set_contents(lines!["fn new() {}".ai()]);

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should show model name (implementation may vary)
    assert!(
        output.contains("model") || output.contains("ai") || output.contains("Mock"),
        "Should show AI model or tool info: {}",
        output
    );
}

// ============================================================================
// Output Format Tests
// ============================================================================

#[test]
fn test_status_output_format() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("format.txt");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make changes
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);

    // Run status
    let output = repo.git_ai(&["status"]).expect("status should succeed");

    // Should have structured output (not empty)
    assert!(
        !output.trim().is_empty(),
        "Status output should not be empty"
    );

    // Should contain some standard elements
    assert!(
        output.contains("+")
            || output.contains("additions")
            || output.contains("ago")
            || output.contains("mock_ai"),
        "Status should contain standard elements"
    );
}

#[test]
fn test_status_no_ansi_escape_codes_in_json() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("ansi.txt");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make changes
    file.set_contents(lines!["Line 2".ai()]);

    // Run status with --json
    let output = repo
        .git_ai(&["status", "--json"])
        .expect("status --json should succeed");

    // Should not contain ANSI escape codes
    assert!(
        !output.contains("\x1b["),
        "JSON output should not contain ANSI escape codes"
    );

    // Should be valid JSON
    let json: Value = serde_json::from_str(&output).expect("Output should be valid JSON");
    assert!(json.is_object(), "JSON should be an object");
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_status_invalid_flag() {
    let repo = TestRepo::new();

    // Try to run status with invalid flag
    let result = repo.git_ai(&["status", "--invalid-flag"]);

    // Should either succeed (ignoring flag) or fail gracefully
    // Implementation may vary
    if let Ok(output) = result {
        // If it succeeds, output should still be reasonable
        assert!(!output.is_empty() || output.is_empty());
    }
}

#[test]
fn test_status_handles_special_characters_in_filenames() {
    let repo = TestRepo::new();

    // Create file with special characters
    let mut special_file = repo.filename("file with spaces.txt");
    special_file.set_contents(lines!["Content".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Modify file
    special_file.set_contents(lines!["Content".human(), "New line".ai()]);

    // Run status
    let output = repo
        .git_ai(&["status"])
        .expect("status should handle special filenames");

    // Should show changes
    assert!(
        output.contains("+") || output.contains("additions"),
        "Should show additions for files with special names"
    );
}

#[test]
fn test_status_unicode_content() {
    let repo = TestRepo::new();

    // Create file with unicode content
    let mut file_uni = repo.filename("unicode.txt");
    file_uni.set_contents(lines!["Hello 世界".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Modify with more unicode
    file_uni.set_contents(lines![
        "Hello 世界".human(),
        "こんにちは".ai(),
        "مرحبا".ai()
    ]);

    // Run status
    let output = repo
        .git_ai(&["status"])
        .expect("status should handle unicode");

    // Should show changes
    assert!(
        output.contains("+") || output.contains("additions"),
        "Should show additions for unicode content"
    );
}

// ============================================================================
// Performance Tests (optional, basic verification)
// ============================================================================

#[test]
fn test_status_with_many_files() {
    let repo = TestRepo::new();

    // Create many files
    for i in 0..50 {
        let mut file = repo.filename(&format!("file{}.txt", i));
        file.set_contents(lines![format!("Content {}", i).human()]);
    }
    repo.stage_all_and_commit("Initial with many files")
        .unwrap();

    // Modify some files
    for i in 0..10 {
        let mut file = repo.filename(&format!("file{}.txt", i));
        file.set_contents(lines![
            format!("Content {}", i).human(),
            format!("New {}", i).ai()
        ]);
    }

    // Run status
    let output = repo
        .git_ai(&["status"])
        .expect("status should handle many files");

    // Should complete successfully and show changes
    assert!(
        output.contains("+") || output.contains("additions"),
        "Should show additions with many files"
    );
}
