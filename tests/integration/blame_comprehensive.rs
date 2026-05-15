use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

// =============================================================================
// 1. Basic mixed authorship
// =============================================================================

#[test]
fn test_blame_basic_mixed_authorship() {
    let repo = TestRepo::new();
    let mut file = repo.filename("file.rs");

    file.set_contents(crate::lines![
        "fn main() {".human(),
        "    let x = ai_generated();".ai(),
        "    println!(\"hello\");".human(),
        "    let y = another_ai_line();".ai()
    ]);

    repo.stage_all_and_commit("Mixed authorship commit")
        .unwrap();

    let blame_output = repo.git_ai(&["blame", "file.rs"]).unwrap();

    // Human lines should show "Test User"
    for line in blame_output.lines() {
        if line.contains("fn main()") || line.contains("println") {
            assert!(
                line.contains("Test User"),
                "Human line should show Test User author, got: {}",
                line
            );
        }
    }

    // AI lines should show "mock_ai"
    for line in blame_output.lines() {
        if line.contains("ai_generated") || line.contains("another_ai_line") {
            assert!(
                line.contains("mock_ai"),
                "AI line should show mock_ai author, got: {}",
                line
            );
        }
    }
}

// =============================================================================
// 2. Only human lines
// =============================================================================

#[test]
fn test_blame_only_human_lines() {
    let repo = TestRepo::new();
    let mut file = repo.filename("human_only.rs");

    file.set_contents(crate::lines![
        "line one".human(),
        "line two".human(),
        "line three".human()
    ]);

    repo.stage_all_and_commit("All human commit").unwrap();

    let blame_output = repo.git_ai(&["blame", "human_only.rs"]).unwrap();

    for line in blame_output.lines() {
        assert!(
            line.contains("Test User"),
            "All lines should show Test User, got: {}",
            line
        );
        assert!(
            !line.contains("mock_ai"),
            "No lines should show mock_ai, got: {}",
            line
        );
    }
}

// =============================================================================
// 3. Only AI lines
// =============================================================================

#[test]
fn test_blame_only_ai_lines() {
    let repo = TestRepo::new();
    let mut file = repo.filename("ai_only.rs");

    file.set_contents(crate::lines![
        "ai line one".ai(),
        "ai line two".ai(),
        "ai line three".ai()
    ]);

    repo.stage_all_and_commit("All AI commit").unwrap();

    let blame_output = repo.git_ai(&["blame", "ai_only.rs"]).unwrap();

    for line in blame_output.lines() {
        assert!(
            line.contains("mock_ai"),
            "All lines should show mock_ai, got: {}",
            line
        );
    }
}

// =============================================================================
// 4. JSON output format
// =============================================================================

#[test]
fn test_blame_json_output_format() {
    let repo = TestRepo::new();
    let mut file = repo.filename("json_test.rs");

    file.set_contents(crate::lines!["human line".human(), "ai line".ai()]);

    repo.stage_all_and_commit("JSON test commit").unwrap();

    let output = repo.git_ai(&["blame", "--json", "json_test.rs"]).unwrap();

    // Should be valid JSON
    let json: serde_json::Value =
        serde_json::from_str(&output).expect("Output should be valid JSON");

    // Should have the expected top-level fields
    assert!(
        json.get("lines").is_some(),
        "JSON should have 'lines' field"
    );
    assert!(
        json.get("prompts").is_some(),
        "JSON should have 'prompts' field"
    );

    // Lines should be an object (AI lines are recorded)
    assert!(json["lines"].is_object(), "lines should be an object");

    // Prompts should be an object
    assert!(json["prompts"].is_object(), "prompts should be an object");

    // At least the lines object should be non-empty (we have an AI line)
    let lines = json["lines"].as_object().unwrap();
    assert!(
        !lines.is_empty(),
        "lines object should not be empty since we have an AI line"
    );
}

// =============================================================================
// 5. Line range
// =============================================================================

#[test]
fn test_blame_line_range() {
    let repo = TestRepo::new();
    let mut file = repo.filename("range.rs");

    file.set_contents(crate::lines![
        "line 1", "line 2", "line 3", "line 4", "line 5"
    ]);

    repo.stage_all_and_commit("Range test commit").unwrap();

    let output = repo.git_ai(&["blame", "-L", "2,4", "range.rs"]).unwrap();

    assert!(output.contains("line 2"), "Should contain line 2");
    assert!(output.contains("line 3"), "Should contain line 3");
    assert!(output.contains("line 4"), "Should contain line 4");
    assert!(!output.contains("line 1"), "Should NOT contain line 1");
    assert!(!output.contains("line 5"), "Should NOT contain line 5");

    // Should have exactly 3 lines of output
    assert_eq!(
        output.lines().count(),
        3,
        "Should have exactly 3 blame lines"
    );
}

// =============================================================================
// 6. Missing file errors
// =============================================================================

#[test]
fn test_blame_missing_file_errors() {
    let repo = TestRepo::new();

    // Create at least one commit so HEAD exists
    let mut file = repo.filename("existing.txt");
    file.set_contents(crate::lines!["content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let result = repo.git_ai(&["blame", "nonexistent.txt"]);

    assert!(
        result.is_err(),
        "Blame on nonexistent file should return error"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("File not found")
            || err.contains("does not exist")
            || err.contains("No such file")
            || err.contains("no such path")
            || err.contains("pathspec")
            || err.contains("canonicalize file path"),
        "Expected error about missing file, got: {}",
        err
    );
}

// =============================================================================
// 7. Empty file
// =============================================================================

#[test]
fn test_blame_empty_file() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("empty.txt");
    fs::write(&file_path, "").unwrap();

    repo.git(&["add", "empty.txt"]).unwrap();
    repo.stage_all_and_commit("Empty file").unwrap();

    // Empty files should either produce no output or return an error
    let result = repo.git_ai(&["blame", "empty.txt"]);
    match result {
        Ok(output) => {
            assert!(
                output.trim().is_empty(),
                "Empty file blame should produce no output, got: {}",
                output
            );
        }
        Err(_) => {
            // An error is also acceptable for empty files (line range 1:0 is invalid)
        }
    }
}

// =============================================================================
// 8. Unicode content
// =============================================================================

#[test]
fn test_blame_unicode_content() {
    let repo = TestRepo::new();
    let mut file = repo.filename("unicode.rs");

    file.set_contents(crate::lines![
        "let greeting = \"Hello 世界\";".ai(),
        "let emoji = \"🚀 🎉\";".human(),
        "let greek = \"αβγδ\";".ai()
    ]);

    repo.stage_all_and_commit("Unicode content").unwrap();

    let blame_output = repo.git_ai(&["blame", "unicode.rs"]).unwrap();

    assert!(
        blame_output.contains("世界"),
        "Should contain Chinese characters"
    );
    assert!(blame_output.contains("🚀"), "Should contain emoji");
    assert!(
        blame_output.contains("αβγδ"),
        "Should contain Greek characters"
    );

    // Verify authorship is correct for unicode lines
    for line in blame_output.lines() {
        if line.contains("世界") || line.contains("αβγδ") {
            assert!(
                line.contains("mock_ai"),
                "Unicode AI line should show mock_ai author, got: {}",
                line
            );
        }
        if line.contains("🚀") {
            assert!(
                line.contains("Test User"),
                "Unicode human line should show Test User, got: {}",
                line
            );
        }
    }
}

// =============================================================================
// 9. Renamed file
// =============================================================================

#[test]
fn test_blame_renamed_file() {
    let repo = TestRepo::new();
    let mut file = repo.filename("original.rs");

    file.set_contents(crate::lines![
        "fn original() {}".ai(),
        "fn helper() {}".human()
    ]);
    repo.stage_all_and_commit("Add original file").unwrap();

    // Rename the file using git mv
    repo.git(&["mv", "original.rs", "renamed.rs"]).unwrap();
    repo.stage_all_and_commit("Rename file").unwrap();

    let blame_output = repo.git_ai(&["blame", "renamed.rs"]).unwrap();

    // Content should still be present and attributable
    assert!(
        blame_output.contains("fn original()"),
        "Renamed file should still show original content"
    );
    assert!(
        blame_output.contains("fn helper()"),
        "Renamed file should still show all content"
    );

    // AI attribution should be preserved through rename
    for line in blame_output.lines() {
        if line.contains("fn original()") {
            assert!(
                line.contains("mock_ai"),
                "AI attribution should survive rename, got: {}",
                line
            );
        }
    }
}

// =============================================================================
// 10. Multiple commits
// =============================================================================

#[test]
fn test_blame_multiple_commits() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("multi.rs");

    // First commit: human-only lines (no checkpoint needed, they'll be untracked/human)
    fs::write(&file_path, "fn first() {}\nfn second() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "multi.rs"])
        .unwrap();
    repo.stage_all_and_commit("First commit").unwrap();

    // Second commit: append an AI line
    fs::write(
        &file_path,
        "fn first() {}\nfn second() {}\nfn ai_added() {}\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "multi.rs"]).unwrap();
    repo.stage_all_and_commit("Second commit").unwrap();

    // Third commit: append a human line
    fs::write(
        &file_path,
        "fn first() {}\nfn second() {}\nfn ai_added() {}\nfn third_human() {}\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "multi.rs"])
        .unwrap();
    repo.stage_all_and_commit("Third commit").unwrap();

    let blame_output = repo.git_ai(&["blame", "multi.rs"]).unwrap();

    // Verify all lines present
    assert!(blame_output.contains("fn first()"));
    assert!(blame_output.contains("fn second()"));
    assert!(blame_output.contains("fn ai_added()"));
    assert!(blame_output.contains("fn third_human()"));

    // Verify AI attribution survived multiple commits
    for line in blame_output.lines() {
        if line.contains("fn ai_added()") {
            assert!(
                line.contains("mock_ai"),
                "AI line should retain mock_ai through multiple commits, got: {}",
                line
            );
        }
        if line.contains("fn first()")
            || line.contains("fn second()")
            || line.contains("fn third_human()")
        {
            assert!(
                line.contains("Test User"),
                "Human line should show Test User, got: {}",
                line
            );
        }
    }
}

// =============================================================================
// 11. After rebase
// =============================================================================

#[test]
fn test_blame_after_rebase() {
    let repo = TestRepo::new();

    // Create initial commit on main
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI content
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.rs");
    feature_file.set_contents(crate::lines![
        "fn feature_human() {}".human(),
        "fn feature_ai() {}".ai()
    ]);
    repo.stage_all_and_commit("Feature commit").unwrap();

    // Advance main with non-conflicting change
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut other_file = repo.filename("other.txt");
    other_file.set_contents(crate::lines!["other content"]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify blame still shows correct authorship after rebase
    let blame_output = repo.git_ai(&["blame", "feature.rs"]).unwrap();

    for line in blame_output.lines() {
        if line.contains("feature_human") {
            assert!(
                line.contains("Test User"),
                "Human line should retain author after rebase, got: {}",
                line
            );
        }
        if line.contains("feature_ai") {
            assert!(
                line.contains("mock_ai"),
                "AI line should retain mock_ai after rebase, got: {}",
                line
            );
        }
    }
}

crate::reuse_tests_in_worktree!(
    test_blame_basic_mixed_authorship,
    test_blame_only_human_lines,
    test_blame_only_ai_lines,
    test_blame_json_output_format,
    test_blame_line_range,
    test_blame_missing_file_errors,
    test_blame_empty_file,
    test_blame_unicode_content,
    test_blame_renamed_file,
    test_blame_multiple_commits,
    test_blame_after_rebase,
);
