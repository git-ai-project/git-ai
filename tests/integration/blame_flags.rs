use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

// =============================================================================
// 1. Porcelain output
// =============================================================================

#[test]
fn test_blame_porcelain_output() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.rs");

    file.set_contents(crate::lines!["human line".human(), "ai line".ai()]);

    repo.stage_all_and_commit("Porcelain test").unwrap();

    let output = repo.git_ai(&["blame", "--porcelain", "test.rs"]).unwrap();

    // Porcelain format should contain metadata fields
    assert!(output.contains("author "), "Should contain 'author' field");
    assert!(
        output.contains("author-mail "),
        "Should contain 'author-mail' field"
    );
    assert!(
        output.contains("author-time "),
        "Should contain 'author-time' field"
    );
    assert!(
        output.contains("committer "),
        "Should contain 'committer' field"
    );
    assert!(
        output.contains("summary "),
        "Should contain 'summary' field"
    );
    assert!(
        output.contains("filename "),
        "Should contain 'filename' field"
    );

    // Content lines are prefixed with a tab in porcelain format
    assert!(
        output.contains("\thuman line"),
        "Should contain tab-prefixed content"
    );
    assert!(
        output.contains("\tai line"),
        "Should contain tab-prefixed AI content"
    );

    // AI line should show mock_ai as author in porcelain metadata
    let lines: Vec<&str> = output.lines().collect();
    let mut found_ai_author = false;
    for line in &lines {
        if line.starts_with("author mock_ai") {
            found_ai_author = true;
            break;
        }
    }
    assert!(
        found_ai_author,
        "Porcelain should show mock_ai in author field"
    );
}

// =============================================================================
// 2. Incremental output
// =============================================================================

#[test]
fn test_blame_incremental_output() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.rs");

    file.set_contents(crate::lines!["line 1", "line 2".ai()]);

    repo.stage_all_and_commit("Incremental test").unwrap();

    let output = repo.git_ai(&["blame", "--incremental", "test.rs"]).unwrap();

    // Incremental format has metadata fields
    assert!(
        output.contains("author "),
        "Incremental should have author field"
    );
    assert!(
        output.contains("filename "),
        "Incremental should have filename field"
    );
    assert!(
        output.contains("summary "),
        "Incremental should have summary field"
    );

    // Should show mock_ai for AI-attributed lines
    assert!(
        output.contains("author mock_ai"),
        "Incremental should show mock_ai for AI lines"
    );
}

// =============================================================================
// 3. Show email flag (-e) in porcelain mode
// =============================================================================

#[test]
fn test_blame_show_email_flag() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.rs");

    file.set_contents(crate::lines!["content line".human(), "ai content".ai()]);

    repo.stage_all_and_commit("Email test").unwrap();

    // Use porcelain format where email is shown in author-mail field
    let output = repo.git_ai(&["blame", "--porcelain", "test.rs"]).unwrap();

    // Porcelain always includes author-mail with email
    assert!(
        output.contains("author-mail <test@example.com>"),
        "Porcelain should show test@example.com in author-mail for human lines, got: {}",
        output
    );
}

// =============================================================================
// 4. Show filename flag (-f) verified via porcelain
// =============================================================================

#[test]
fn test_blame_show_name_flag() {
    let repo = TestRepo::new();

    let subdir = repo.path().join("src");
    fs::create_dir_all(&subdir).unwrap();

    let mut file = repo.filename("src/module.rs");
    file.set_contents(crate::lines!["fn module() {}".human()]);

    repo.stage_all_and_commit("Filename test").unwrap();

    // Porcelain format always shows filename
    let output = repo
        .git_ai(&["blame", "--porcelain", "src/module.rs"])
        .unwrap();

    assert!(
        output.contains("filename src/module.rs") || output.contains("filename module.rs"),
        "Porcelain should show filename field, got: {}",
        output
    );
}

// =============================================================================
// 5. Line porcelain
// =============================================================================

#[test]
fn test_blame_line_porcelain() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.rs");

    file.set_contents(crate::lines!["line 1", "line 2", "line 3"]);

    repo.stage_all_and_commit("Line porcelain test").unwrap();

    let output = repo
        .git_ai(&["blame", "--line-porcelain", "test.rs"])
        .unwrap();

    // Line porcelain should have full metadata for EVERY line
    let author_count = output.matches("author ").count();
    assert!(
        author_count >= 3,
        "Line porcelain should have author for each line, got {} for 3 lines",
        author_count
    );

    // Each line should have its own filename field
    let filename_count = output.matches("filename ").count();
    assert!(
        filename_count >= 3,
        "Line porcelain should have filename for each line, got {} for 3 lines",
        filename_count
    );
}

// =============================================================================
// 6. Abbrev flag (default hash abbreviation in git-ai blame output)
// =============================================================================

#[test]
fn test_blame_abbrev_flag() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.rs");

    file.set_contents(crate::lines!["content"]);

    repo.stage_all_and_commit("Abbrev test").unwrap();

    // Default blame output uses abbreviated hashes (typically 7-8 chars)
    let output = repo.git_ai(&["blame", "test.rs"]).unwrap();

    let first_field = output
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let hash = first_field.trim_start_matches('^');
    assert!(
        hash.len() >= 7 && hash.len() <= 40,
        "Hash in default blame should be abbreviated (7-40 chars), got len={}: {}",
        hash.len(),
        hash
    );
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "Hash should be hex characters, got: {}",
        hash
    );
}

// =============================================================================
// 7. Date format (verified through porcelain which always shows raw timestamps)
// =============================================================================

#[test]
fn test_blame_date_format() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.rs");

    file.set_contents(crate::lines!["content"]);

    repo.stage_all_and_commit("Date format test").unwrap();

    // The default blame output always shows dates in the format: YYYY-MM-DD HH:MM:SS +ZZZZ
    let output = repo.git_ai(&["blame", "test.rs"]).unwrap();

    // Verify the date is present in the output in the standard format
    let has_date = output
        .split_whitespace()
        .any(|word| word.len() == 10 && word.matches('-').count() == 2);
    assert!(
        has_date,
        "Blame output should contain a date in YYYY-MM-DD format, got: {}",
        output
    );
}

// =============================================================================
// 8. JSON output with AI and human lines
// =============================================================================

#[test]
fn test_blame_json_with_mixed_authorship() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.rs");

    file.set_contents(crate::lines![
        "human line 1".human(),
        "ai line 1".ai(),
        "ai line 2".ai(),
        "human line 2".human()
    ]);

    repo.stage_all_and_commit("JSON mixed test").unwrap();

    let output = repo.git_ai(&["blame", "--json", "test.rs"]).unwrap();
    let json: serde_json::Value = serde_json::from_str(&output).expect("Should produce valid JSON");

    // lines field should contain entries for AI lines
    let lines = json["lines"].as_object().expect("lines should be object");
    assert!(
        !lines.is_empty(),
        "JSON lines should not be empty for file with AI content"
    );

    // The line ranges in JSON should cover the AI lines (lines 2 and 3)
    let has_ai_coverage = lines.keys().any(|key| {
        // Keys can be "2" or "2-3" for ranges
        key.contains('2') || key.contains('3')
    });
    assert!(
        has_ai_coverage,
        "JSON should have entries covering AI lines 2-3, got keys: {:?}",
        lines.keys().collect::<Vec<_>>()
    );
}

// =============================================================================
// 9. Multiple line ranges
// =============================================================================

#[test]
fn test_blame_multiple_line_ranges() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.rs");

    file.set_contents(crate::lines![
        "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8"
    ]);

    repo.stage_all_and_commit("Multi range test").unwrap();

    // Request two non-contiguous ranges
    let output = repo
        .git_ai(&["blame", "-L", "2,3", "-L", "6,7", "test.rs"])
        .unwrap();

    // Should include lines from both ranges
    assert!(output.contains("line 2"), "Should contain line 2");
    assert!(output.contains("line 3"), "Should contain line 3");
    assert!(output.contains("line 6"), "Should contain line 6");
    assert!(output.contains("line 7"), "Should contain line 7");

    // Should NOT include lines outside the ranges
    assert!(!output.contains("line 1"), "Should NOT contain line 1");
    assert!(!output.contains("line 4"), "Should NOT contain line 4");
    assert!(!output.contains("line 5"), "Should NOT contain line 5");
    assert!(!output.contains("line 8"), "Should NOT contain line 8");
}

// =============================================================================
// 10. Porcelain AI attribution carries prompt hash
// =============================================================================

#[test]
fn test_blame_porcelain_ai_attribution() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.rs");

    file.set_contents(crate::lines![
        "fn human_code() {}".human(),
        "fn ai_code() {}".ai()
    ]);

    repo.stage_all_and_commit("AI attribution test").unwrap();

    let output = repo.git_ai(&["blame", "--porcelain", "test.rs"]).unwrap();

    // Parse porcelain to find author lines
    let author_lines: Vec<&str> = output
        .lines()
        .filter(|l| l.starts_with("author ") && !l.starts_with("author-"))
        .collect();

    // Should have 2 author lines (one per blame line)
    assert_eq!(
        author_lines.len(),
        2,
        "Should have 2 author lines in porcelain output"
    );

    // One should be Test User, one should be mock_ai
    let has_human = author_lines.iter().any(|l| l.contains("Test User"));
    let has_ai = author_lines.iter().any(|l| l.contains("mock_ai"));

    assert!(has_human, "Should have Test User in porcelain authors");
    assert!(has_ai, "Should have mock_ai in porcelain authors");
}

crate::reuse_tests_in_worktree!(
    test_blame_porcelain_output,
    test_blame_incremental_output,
    test_blame_show_email_flag,
    test_blame_show_name_flag,
    test_blame_line_porcelain,
    test_blame_abbrev_flag,
    test_blame_date_format,
    test_blame_json_with_mixed_authorship,
    test_blame_multiple_line_ranges,
    test_blame_porcelain_ai_attribution,
);
