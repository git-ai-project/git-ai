use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// Extract the first complete JSON object from mixed stdout/stderr output.
fn extract_json_object(output: &str) -> String {
    let start = output.find('{').unwrap_or(0);
    let end = output.rfind('}').unwrap_or(output.len().saturating_sub(1));
    output[start..=end].to_string()
}

// =============================================================================
// 1. Basic mixed commit stats
// =============================================================================

#[test]
fn test_stats_basic_mixed_commit() {
    let repo = TestRepo::new();

    // Initial commit so HEAD exists
    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Mixed commit with both human and AI lines
    let mut file = repo.filename("code.rs");
    file.set_contents(crate::lines![
        "fn human_fn() {}".human(),
        "fn ai_fn() {}".ai(),
        "fn another_human() {}".human(),
        "fn another_ai() {}".ai(),
        "fn third_ai() {}".ai()
    ]);
    repo.stage_all_and_commit("Mixed commit").unwrap();

    let output = repo.git_ai(&["stats", "--json", "HEAD"]).unwrap();
    let json_str = extract_json_object(&output);
    let json: serde_json::Value =
        serde_json::from_str(&json_str).expect("Should produce valid stats JSON");

    // Should have AI additions (at least 3 AI lines)
    let ai_additions = json["ai_additions"].as_u64().unwrap_or(0);
    assert!(
        ai_additions >= 3,
        "Should have at least 3 AI additions, got: {}",
        ai_additions
    );

    // Total added lines in the diff should be 5
    let git_diff_added = json["git_diff_added_lines"].as_u64().unwrap_or(0);
    assert_eq!(git_diff_added, 5, "Should have 5 total added lines in diff");
}

// =============================================================================
// 2. JSON output format
// =============================================================================

#[test]
fn test_stats_json_output() {
    let repo = TestRepo::new();

    let mut file = repo.filename("main.rs");
    file.set_contents(crate::lines![
        "fn main() {}".human(),
        "fn ai_helper() {}".ai()
    ]);
    repo.stage_all_and_commit("Stats JSON test").unwrap();

    let output = repo.git_ai(&["stats", "--json", "HEAD"]).unwrap();
    let json_str = extract_json_object(&output);
    let json: serde_json::Value =
        serde_json::from_str(&json_str).expect("Should produce valid JSON");

    // Verify expected fields exist
    assert!(
        json.get("ai_additions").is_some(),
        "Should have 'ai_additions' field"
    );
    assert!(
        json.get("human_additions").is_some() || json.get("unknown_additions").is_some(),
        "Should have human or unknown additions field"
    );
    assert!(
        json.get("git_diff_added_lines").is_some(),
        "Should have 'git_diff_added_lines' field"
    );
    assert!(
        json.get("git_diff_deleted_lines").is_some(),
        "Should have 'git_diff_deleted_lines' field"
    );
}

// =============================================================================
// 3. Commit range stats
// =============================================================================

#[test]
fn test_stats_commit_range() {
    let repo = TestRepo::new();

    // First commit: human lines
    let mut file = repo.filename("range.rs");
    file.set_contents(crate::lines!["fn first() {}".human()]);
    let first = repo.stage_all_and_commit("First commit").unwrap();

    // Second commit: AI adds a line
    file.set_contents(crate::lines![
        "fn first() {}".human(),
        "fn ai_second() {}".ai()
    ]);
    let second = repo.stage_all_and_commit("Second commit").unwrap();

    // Query stats for just the range between first and second
    let range = format!("{}..{}", first.commit_sha, second.commit_sha);
    let output = repo
        .git_ai(&["stats", &range, "--json"])
        .expect("stats for range should succeed");
    let json_str = extract_json_object(&output);
    let json: serde_json::Value =
        serde_json::from_str(&json_str).expect("Range stats should be valid JSON");

    // The range should cover only the second commit's changes
    // Check nested range_stats if present, or top-level fields
    let stats = if json.get("range_stats").is_some() {
        json["range_stats"].clone()
    } else {
        json.clone()
    };

    let ai_additions = stats["ai_additions"].as_u64().unwrap_or(0);
    assert!(
        ai_additions >= 1,
        "Range should show at least 1 AI addition, got: {}",
        ai_additions
    );

    let added_lines = stats["git_diff_added_lines"].as_u64().unwrap_or(0);
    assert!(
        added_lines >= 1,
        "Range should have at least 1 added line, got: {}",
        added_lines
    );
}

// =============================================================================
// 4. Ignores lockfiles
// =============================================================================

#[test]
fn test_stats_ignores_lockfiles() {
    let repo = TestRepo::new();

    // Initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Commit with source file and lockfile
    let mut source = repo.filename("src/lib.rs");
    source.set_contents(crate::lines!["pub fn answer() -> u32 { 42 }".ai()]);

    // Create a large lockfile (simulating package-lock.json or Cargo.lock)
    repo.filename("Cargo.lock")
        .set_contents(vec!["lockfile-entry".to_string(); 500]);

    repo.stage_all_and_commit("Add source and lockfile")
        .unwrap();

    let output = repo.git_ai(&["stats", "--json", "HEAD"]).unwrap();
    let json_str = extract_json_object(&output);
    let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    // Stats should only count the source file, not the lockfile
    let added_lines = json["git_diff_added_lines"].as_u64().unwrap_or(0);
    assert_eq!(
        added_lines, 1,
        "Stats should exclude lockfile lines, only counting source file (1 line). Got: {}",
        added_lines
    );
}

// =============================================================================
// 5. Ignores generated files
// =============================================================================

#[test]
fn test_stats_ignores_generated_files() {
    let repo = TestRepo::new();

    // Initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create source file and a generated file
    let mut source = repo.filename("src/app.rs");
    source.set_contents(crate::lines!["fn app() {}".ai()]);

    repo.filename("api.generated.ts")
        .set_contents(vec!["export type X = string;".to_string(); 300]);

    repo.stage_all_and_commit("Add source and generated file")
        .unwrap();

    let output = repo.git_ai(&["stats", "--json", "HEAD"]).unwrap();
    let json_str = extract_json_object(&output);
    let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    // Stats should only count the source file, not the generated file
    let added_lines = json["git_diff_added_lines"].as_u64().unwrap_or(0);
    assert_eq!(
        added_lines, 1,
        "Stats should exclude generated file lines. Got: {}",
        added_lines
    );
}

// =============================================================================
// 6. Multiple files aggregation
// =============================================================================

#[test]
fn test_stats_multiple_files() {
    let repo = TestRepo::new();

    // Initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create multiple source files in one commit
    let mut file1 = repo.filename("src/one.rs");
    file1.set_contents(crate::lines![
        "fn one_human() {}".human(),
        "fn one_ai() {}".ai()
    ]);

    let mut file2 = repo.filename("src/two.rs");
    file2.set_contents(crate::lines![
        "fn two_ai_a() {}".ai(),
        "fn two_ai_b() {}".ai()
    ]);

    let mut file3 = repo.filename("src/three.rs");
    file3.set_contents(crate::lines!["fn three_human() {}".human()]);

    repo.stage_all_and_commit("Add multiple files").unwrap();

    let output = repo.git_ai(&["stats", "--json", "HEAD"]).unwrap();
    let json_str = extract_json_object(&output);
    let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    // Total diff should be 5 lines across all files
    let added_lines = json["git_diff_added_lines"].as_u64().unwrap_or(0);
    assert_eq!(
        added_lines, 5,
        "Stats should aggregate 5 lines across all files. Got: {}",
        added_lines
    );

    // AI additions should be 3 (one_ai + two_ai_a + two_ai_b)
    let ai_additions = json["ai_additions"].as_u64().unwrap_or(0);
    assert_eq!(
        ai_additions, 3,
        "Should have 3 AI additions across files. Got: {}",
        ai_additions
    );
}

// =============================================================================
// 7. Stats after rebase
// =============================================================================

#[test]
fn test_stats_after_rebase() {
    let repo = TestRepo::new();

    // Initial commit on main
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI content
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.rs");
    feature_file.set_contents(crate::lines![
        "fn feature_ai() {}".ai(),
        "fn feature_human() {}".human()
    ]);
    repo.stage_all_and_commit("Feature commit").unwrap();

    // Advance main with non-conflicting change
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut other_file = repo.filename("other.txt");
    other_file.set_contents(crate::lines!["other content"]);
    repo.stage_all_and_commit("Main advance").unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Stats should still be correct after rebase
    let output = repo.git_ai(&["stats", "--json", "HEAD"]).unwrap();
    let json_str = extract_json_object(&output);
    let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    let added_lines = json["git_diff_added_lines"].as_u64().unwrap_or(0);
    assert_eq!(
        added_lines, 2,
        "After rebase, stats should show 2 added lines. Got: {}",
        added_lines
    );

    let ai_additions = json["ai_additions"].as_u64().unwrap_or(0);
    assert!(
        ai_additions >= 1,
        "After rebase, should still have at least 1 AI addition. Got: {}",
        ai_additions
    );
}

// =============================================================================
// 8. Empty commit
// =============================================================================

#[test]
fn test_stats_empty_commit() {
    let repo = TestRepo::new();

    // Create a file and commit it
    let mut file = repo.filename("file.rs");
    file.set_contents(crate::lines!["content"]);
    repo.stage_all_and_commit("Initial commit with content")
        .unwrap();

    // Create an empty commit (no file changes)
    repo.git(&["commit", "--allow-empty", "-m", "Empty commit"])
        .unwrap();
    // Run post-commit for the empty commit
    let _ = repo.git_ai(&["post-commit"]);

    let output = repo.git_ai(&["stats", "--json", "HEAD"]).unwrap();
    let json_str = extract_json_object(&output);
    let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    // Empty commit should show zero additions
    let added_lines = json["git_diff_added_lines"].as_u64().unwrap_or(0);
    assert_eq!(
        added_lines, 0,
        "Empty commit should have 0 added lines. Got: {}",
        added_lines
    );

    let ai_additions = json["ai_additions"].as_u64().unwrap_or(0);
    assert_eq!(
        ai_additions, 0,
        "Empty commit should have 0 AI additions. Got: {}",
        ai_additions
    );

    let deleted_lines = json["git_diff_deleted_lines"].as_u64().unwrap_or(0);
    assert_eq!(
        deleted_lines, 0,
        "Empty commit should have 0 deleted lines. Got: {}",
        deleted_lines
    );
}

crate::reuse_tests_in_worktree!(
    test_stats_basic_mixed_commit,
    test_stats_json_output,
    test_stats_commit_range,
    test_stats_ignores_lockfiles,
    test_stats_ignores_generated_files,
    test_stats_multiple_files,
    test_stats_after_rebase,
    test_stats_empty_commit,
);
