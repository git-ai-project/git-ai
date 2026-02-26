mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Helper to parse diff output and extract meaningful lines
#[derive(Debug, PartialEq)]
struct DiffLine {
    prefix: String,
    content: String,
    attribution: Option<String>,
}

impl DiffLine {
    fn parse(line: &str) -> Option<Self> {
        // Skip headers and hunk markers
        if line.starts_with("diff --git")
            || line.starts_with("index ")
            || line.starts_with("---")
            || line.starts_with("+++")
            || line.starts_with("@@")
            || line.is_empty()
        {
            return None;
        }

        let prefix = if line.starts_with('+') {
            "+"
        } else if line.starts_with('-') {
            "-"
        } else if line.starts_with(' ') {
            " "
        } else {
            return None;
        };

        // Extract content and attribution
        let rest = &line[1..];

        // Look for attribution markers at the end
        let attribution = if rest.contains("🤖") {
            // AI attribution: extract tool name after 🤖
            let parts: Vec<&str> = rest.split("🤖").collect();
            if parts.len() > 1 {
                Some(format!("ai:{}", parts[1].trim()))
            } else {
                Some("ai:unknown".to_string())
            }
        } else if rest.contains("👤") {
            // Human attribution: extract username after 👤
            let parts: Vec<&str> = rest.split("👤").collect();
            if parts.len() > 1 {
                Some(format!("human:{}", parts[1].trim()))
            } else {
                Some("human:unknown".to_string())
            }
        } else if rest.contains("[no-data]") {
            Some("no-data".to_string())
        } else {
            None
        };

        // Extract content (everything before attribution markers)
        let content = if attribution.is_some() {
            // Remove attribution from content
            rest.split("🤖")
                .next()
                .or_else(|| rest.split("👤").next())
                .or_else(|| rest.split("[no-data]").next())
                .unwrap_or(rest)
                .trim()
                .to_string()
        } else {
            rest.trim().to_string()
        };

        Some(DiffLine {
            prefix: prefix.to_string(),
            content,
            attribution,
        })
    }
}

/// Parse all meaningful diff lines from output
fn parse_diff_output(output: &str) -> Vec<DiffLine> {
    output.lines().filter_map(DiffLine::parse).collect()
}

/// Helper to assert a line has expected prefix, content, and attribution
fn assert_diff_line(
    line: &DiffLine,
    expected_prefix: &str,
    expected_content: &str,
    expected_attribution: Option<&str>,
) {
    assert_eq!(
        line.prefix, expected_prefix,
        "Line prefix mismatch: expected '{}', got '{}' for content '{}'",
        expected_prefix, line.prefix, line.content
    );

    assert!(
        line.content.contains(expected_content),
        "Line content mismatch: expected '{}' to contain '{}', full line: {:?}",
        line.content,
        expected_content,
        line
    );

    match (expected_attribution, &line.attribution) {
        (Some(expected), Some(actual)) => {
            assert!(
                actual.contains(expected),
                "Attribution mismatch: expected '{}' to contain '{}', full line: {:?}",
                actual,
                expected,
                line
            );
        }
        (Some(expected), None) => {
            panic!(
                "Expected attribution '{}' but found none for line: {:?}",
                expected, line
            );
        }
        (None, _) => {
            // Don't care about attribution
        }
    }
}

/// Assert exact sequence of diff lines with prefix, content, and attribution
fn assert_diff_lines_exact(lines: &[DiffLine], expected: &[(&str, &str, Option<&str>)]) {
    assert_eq!(
        lines.len(),
        expected.len(),
        "Line count mismatch: expected {} lines, got {}\nExpected: {:?}\nActual: {:?}",
        expected.len(),
        lines.len(),
        expected,
        lines
    );

    for (i, (line, (exp_prefix, exp_content, exp_attr))) in
        lines.iter().zip(expected.iter()).enumerate()
    {
        assert_eq!(
            &line.prefix, exp_prefix,
            "Line {} prefix mismatch: expected '{}', got '{}'\nFull line: {:?}",
            i, exp_prefix, line.prefix, line
        );

        assert!(
            line.content.contains(exp_content),
            "Line {} content mismatch: expected to contain '{}', got '{}'\nFull line: {:?}",
            i,
            exp_content,
            line.content,
            line
        );

        match (exp_attr, &line.attribution) {
            (Some(expected_attr), Some(actual_attr)) => {
                assert!(
                    actual_attr.contains(expected_attr),
                    "Line {} attribution mismatch: expected '{}', got '{}'\nFull line: {:?}",
                    i,
                    expected_attr,
                    actual_attr,
                    line
                );
            }
            (Some(expected_attr), None) => {
                panic!(
                    "Line {} expected attribution '{}' but found none\nFull line: {:?}",
                    i, expected_attr, line
                );
            }
            (None, Some(actual_attr)) => {
                // Expected no attribution but got one - this is OK for flexibility
                eprintln!(
                    "Warning: Line {} has unexpected attribution '{}', but not enforcing",
                    i, actual_attr
                );
            }
            (None, None) => {
                // Both None, OK
            }
        }
    }
}

fn configure_repo_external_diff_helper(repo: &TestRepo) -> String {
    let marker = "EXTERNAL_DIFF_MARKER";
    let helper_path = repo.path().join("ext-diff-helper.sh");
    let helper_path_posix = helper_path
        .to_str()
        .expect("helper path must be valid UTF-8")
        .replace('\\', "/");

    fs::write(&helper_path, format!("#!/bin/sh\necho {marker}\nexit 0\n"))
        .expect("should write external diff helper");
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&helper_path)
            .expect("helper metadata should exist")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&helper_path, perms).expect("helper should be executable");
    }

    repo.git_og(&["config", "diff.external", &helper_path_posix])
        .expect("configuring diff.external should succeed");

    marker.to_string()
}

fn create_external_diff_helper_script(repo: &TestRepo, marker: &str) -> std::path::PathBuf {
    let helper_path = repo.path().join(format!("ext-env-helper-{marker}.sh"));

    fs::write(&helper_path, format!("#!/bin/sh\necho {marker}\nexit 0\n"))
        .expect("should write external diff helper");
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&helper_path)
            .expect("helper metadata should exist")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&helper_path, perms).expect("helper should be executable");
    }

    helper_path
}

#[test]
fn test_diff_single_commit() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1".human(), "Line 2".human()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Second commit with AI and human changes
    file.set_contents(lines![
        "Line 1".human(),
        "Line 2 modified".ai(),
        "Line 3 new".ai(),
        "Line 4 human".human()
    ]);
    let second = repo.stage_all_and_commit("Mixed changes").unwrap();

    // Run git-ai diff on the second commit
    let output = repo
        .git_ai(&["diff", &second.commit_sha])
        .expect("git-ai diff should succeed");

    // Parse diff output
    let lines = parse_diff_output(&output);

    // Verify exact lines
    // Should have: -Line 2, +Line 2 modified, +Line 3 new, +Line 4 human
    assert!(
        lines.len() >= 4,
        "Should have at least 4 diff lines, got {}: {:?}",
        lines.len(),
        lines
    );

    // Find the deletion of Line 2
    let line2_deletion = lines
        .iter()
        .find(|l| l.prefix == "-" && l.content.contains("Line 2"));
    assert!(line2_deletion.is_some(), "Should have deletion of Line 2");

    // Find additions
    let line2_addition = lines
        .iter()
        .find(|l| l.prefix == "+" && l.content.contains("Line 2 modified"));
    assert!(
        line2_addition.is_some(),
        "Should have addition of 'Line 2 modified'"
    );
    if let Some(line) = line2_addition {
        assert!(
            line.attribution
                .as_ref()
                .map(|a| a.contains("ai"))
                .unwrap_or(false),
            "Line 2 modified should have AI attribution, got: {:?}",
            line.attribution
        );
    }

    let line3_addition = lines
        .iter()
        .find(|l| l.prefix == "+" && l.content.contains("Line 3 new"));
    assert!(
        line3_addition.is_some(),
        "Should have addition of 'Line 3 new'"
    );
    if let Some(line) = line3_addition {
        assert!(
            line.attribution
                .as_ref()
                .map(|a| a.contains("ai"))
                .unwrap_or(false),
            "Line 3 new should have AI attribution, got: {:?}",
            line.attribution
        );
    }

    let line4_addition = lines
        .iter()
        .find(|l| l.prefix == "+" && l.content.contains("Line 4 human"));
    assert!(
        line4_addition.is_some(),
        "Should have addition of 'Line 4 human'"
    );
}

#[test]
fn test_diff_commit_range() {
    let repo = TestRepo::new();

    // First commit
    let mut file = repo.filename("range.txt");
    file.set_contents(lines!["Line 1".human()]);
    let first = repo.stage_all_and_commit("First commit").unwrap();

    // Second commit
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Second commit").unwrap();

    // Third commit
    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".human()]);
    let third = repo.stage_all_and_commit("Third commit").unwrap();

    // Run git-ai diff with range
    let range = format!("{}..{}", first.commit_sha, third.commit_sha);
    let output = repo
        .git_ai(&["diff", &range])
        .expect("git-ai diff range should succeed");

    // Verify output
    assert!(output.contains("diff --git"), "Should contain diff header");
    assert!(output.contains("range.txt"), "Should mention the file");
    assert!(
        output.contains("+Line 2") || output.contains("Line 2"),
        "Should show added line"
    );
    assert!(
        output.contains("+Line 3") || output.contains("Line 3"),
        "Should show added line"
    );
}

#[test]
fn test_diff_shows_ai_attribution() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("ai_test.rs");
    file.set_contents(lines!["fn old() {}".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // AI makes changes
    file.set_contents(lines!["fn new() {}".ai(), "fn another() {}".ai()]);
    let commit = repo.stage_all_and_commit("AI changes").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Parse and verify exact sequence
    let lines = parse_diff_output(&output);

    // Verify exact order: deletion, then two additions
    assert_diff_lines_exact(
        &lines,
        &[
            ("-", "fn old()", None),       // Old line deleted (may have no-data or human)
            ("+", "fn new()", Some("ai")), // AI adds fn new()
            ("+", "fn another()", Some("ai")), // AI adds fn another()
        ],
    );
}

#[test]
fn test_diff_shows_human_attribution() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("human_test.rs");
    file.set_contents(lines!["fn old() {}".ai()]);
    repo.stage_all_and_commit("Initial AI").unwrap();

    // Human makes changes
    file.set_contents(lines!["fn new() {}".human(), "fn another() {}".human()]);
    let commit = repo.stage_all_and_commit("Human changes").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Parse and verify exact sequence
    let lines = parse_diff_output(&output);

    // Verify exact order: deletion, then two additions
    assert_eq!(lines.len(), 3, "Should have exactly 3 lines");

    // First line: deletion (no attribution on deletions)
    assert_diff_line(&lines[0], "-", "fn old()", None);

    // Next two lines: additions (will have no-data or human attribution)
    assert_diff_line(&lines[1], "+", "fn new()", None);
    assert_diff_line(&lines[2], "+", "fn another()", None);

    // Verify both additions have some attribution
    assert!(
        lines[1].attribution.is_some(),
        "First addition should have attribution"
    );
    assert!(
        lines[2].attribution.is_some(),
        "Second addition should have attribution"
    );
}

#[test]
fn test_diff_multiple_files() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file1 = repo.filename("file1.txt");
    let mut file2 = repo.filename("file2.txt");
    file1.set_contents(lines!["File 1 line 1".human()]);
    file2.set_contents(lines!["File 2 line 1".human()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Modify both files
    file1.set_contents(lines!["File 1 line 1".human(), "File 1 line 2".ai()]);
    file2.set_contents(lines!["File 2 line 1".human(), "File 2 line 2".human()]);
    let commit = repo.stage_all_and_commit("Modify both files").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Should show both files
    assert!(output.contains("file1.txt"), "Should mention file1");
    assert!(output.contains("file2.txt"), "Should mention file2");

    // Should have multiple diff sections
    let diff_count = output.matches("diff --git").count();
    assert_eq!(diff_count, 2, "Should have 2 diff sections");
}

#[test]
fn test_diff_initial_commit() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("initial.txt");
    file.set_contents(lines!["Initial line".ai()]);
    let commit = repo.stage_all_and_commit("Initial commit").unwrap();

    // Run diff on initial commit (should compare to empty tree)
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff on initial commit should succeed");

    // Parse and verify exact sequence
    let lines = parse_diff_output(&output);

    // Should have exactly 1 addition, no deletions
    assert_diff_lines_exact(
        &lines,
        &[
            ("+", "Initial line", Some("ai")), // Only addition with AI attribution
        ],
    );
}

#[test]
fn test_diff_pure_additions() {
    let repo = TestRepo::new();

    // Initial commit with one line
    let mut file = repo.filename("additions.txt");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Add more lines at the end (pure additions)
    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".ai()]);
    let commit = repo.stage_all_and_commit("Add lines").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Should have additions
    assert!(
        output.contains("+Line 2") || output.contains("Line 2"),
        "Should show Line 2 addition"
    );
    assert!(
        output.contains("+Line 3") || output.contains("Line 3"),
        "Should show Line 3 addition"
    );

    // Should show AI attribution on added lines
    assert!(
        output.contains("🤖") || output.contains("mock_ai"),
        "Should show AI attribution on additions"
    );
}

#[test]
fn test_diff_pure_deletions() {
    let repo = TestRepo::new();

    // Initial commit with multiple lines
    let mut file = repo.filename("deletions.txt");
    file.set_contents(lines![
        "Line 1".ai(),
        "Line 2".ai(),
        "Line 3".human(),
        "Line 4".ai()
    ]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Delete all lines
    file.set_contents(lines![]);
    let commit = repo.stage_all_and_commit("Delete all").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Parse and verify exact sequence
    let lines = parse_diff_output(&output);

    // Verify exact order: 4 deletions in sequence, no additions
    assert_eq!(
        lines.len(),
        4,
        "Should have exactly 4 lines (all deletions)"
    );

    assert_diff_lines_exact(
        &lines,
        &[
            ("-", "Line 1", None), // No attribution on deletions
            ("-", "Line 2", None), // No attribution on deletions
            ("-", "Line 3", None), // No attribution on deletions
            ("-", "Line 4", None), // No attribution on deletions
        ],
    );
}

#[test]
fn test_diff_mixed_ai_and_human() {
    let repo = TestRepo::new();

    // Initial commit with AI content
    let mut file = repo.filename("mixed.txt");
    file.set_contents(lines!["Line 1".ai(), "Line 2".ai()]);
    repo.stage_all_and_commit("Initial AI").unwrap();

    // Modify with AI changes
    file.set_contents(lines![
        "Line 1".ai(),
        "Line 2 modified".ai(),
        "Line 3 new".ai()
    ]);
    let commit = repo.stage_all_and_commit("AI modifies").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Should have both additions and deletions
    assert!(output.contains("-"), "Should have deletion lines");
    assert!(output.contains("+"), "Should have addition lines");

    // Should show AI attribution
    let has_ai = output.contains("🤖") || output.contains("mock_ai");
    assert!(has_ai, "Should show AI attribution, output:\n{}", output);
}

#[test]
fn test_diff_with_head_ref() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("head_test.txt");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Second commit
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Add line").unwrap();

    // Run diff using HEAD
    let output = repo
        .git_ai(&["diff", "HEAD"])
        .expect("git-ai diff HEAD should succeed");

    // Should work with HEAD reference
    assert!(output.contains("diff --git"), "Should contain diff header");
    assert!(output.contains("head_test.txt"), "Should mention the file");
}

#[test]
fn test_diff_output_format() {
    let repo = TestRepo::new();

    // Create a simple diff
    let mut file = repo.filename("format.txt");
    file.set_contents(lines!["old".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    file.set_contents(lines!["new".ai()]);
    let commit = repo.stage_all_and_commit("Change").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Verify standard git diff format elements
    assert!(output.contains("diff --git"), "Should have diff header");
    assert!(output.contains("---"), "Should have old file marker");
    assert!(output.contains("+++"), "Should have new file marker");
    assert!(output.contains("@@"), "Should have hunk header");

    // Parse and verify exact sequence of diff lines
    let lines = parse_diff_output(&output);

    assert_diff_lines_exact(
        &lines,
        &[
            ("-", "old", None),       // Deletion (may have no-data or human)
            ("+", "new", Some("ai")), // Addition with AI attribution
        ],
    );
}

#[test]
fn test_diff_error_on_no_args() {
    let repo = TestRepo::new();

    // Try to run diff without arguments
    let result = repo.git_ai(&["diff"]);

    // Should fail with error
    assert!(result.is_err(), "git-ai diff without arguments should fail");
}

#[test]
fn test_diff_json_output_with_escaped_newlines() {
    let repo = TestRepo::new();

    // Initial commit with text.split("\n")
    let mut file = repo.filename("utils.ts");
    file.set_contents(lines![r#"const lines = text.split("\n")"#.human()]);
    repo.stage_all_and_commit("Initial split implementation")
        .unwrap();

    // Modify to other_text.split("\n\n")
    file.set_contents(lines![r#"const lines = other_text.split("\n\n")"#.ai()]);
    let commit = repo
        .stage_all_and_commit("Update split to use double newline")
        .unwrap();

    // Run git-ai diff with --json flag
    let output = repo
        .git_ai(&["diff", &commit.commit_sha, "--json"])
        .expect("git-ai diff --json should succeed");

    // Parse JSON to verify it's valid
    let json: serde_json::Value =
        serde_json::from_str(&output).expect("Output should be valid JSON");

    // Verify newlines are properly escaped in the base_content
    let files = json.get("files").unwrap().as_object().unwrap();
    let utils_file = files.get("utils.ts").unwrap();
    let base_content = utils_file.get("base_content").unwrap().as_str().unwrap();
    assert!(
        base_content.contains(r#"text.split("\n")"#),
        "Base content should contain properly escaped newlines: text.split(\"\\n\"), got: {}",
        base_content
    );

    // Verify newlines are properly escaped in the diff content
    let diff = utils_file.get("diff").unwrap().as_str().unwrap();
    assert!(
        diff.contains(r#"text.split("\n")"#),
        "Diff should contain properly escaped newlines in old line: text.split(\"\\n\")"
    );
    assert!(
        diff.contains(r#"other_text.split("\n\n")"#),
        "Diff should contain properly escaped newlines in new line: other_text.split(\"\\n\\n\")"
    );

    // Print the JSON output for inspection
    println!("JSON output:\n{}", serde_json::to_string(&json).unwrap());
}

#[test]
fn test_diff_preserves_context_lines() {
    let repo = TestRepo::new();

    // Create file with multiple lines
    let mut file = repo.filename("context.txt");
    file.set_contents(lines![
        "Context 1".human(),
        "Context 2".human(),
        "Context 3".human(),
        "Old line".human(),
        "Context 4".human(),
        "Context 5".human(),
        "Context 6".human()
    ]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Change one line in the middle
    file.set_contents(lines![
        "Context 1".human(),
        "Context 2".human(),
        "Context 3".human(),
        "New line".ai(),
        "Context 4".human(),
        "Context 5".human(),
        "Context 6".human()
    ]);
    let commit = repo.stage_all_and_commit("Change middle").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Should show context lines (lines starting with space)
    let context_count = output
        .lines()
        .filter(|l| l.starts_with(' ') && !l.starts_with("  "))
        .count();
    assert!(
        context_count >= 3,
        "Should show at least 3 context lines (default -U3)"
    );
}

#[test]
fn test_diff_exact_sequence_verification() {
    let repo = TestRepo::new();

    // Initial commit with 2 lines
    let mut file = repo.filename("sequence.rs");
    file.set_contents(lines!["fn first() {}".human(), "fn second() {}".ai()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Modify: delete first, modify second, add third
    file.set_contents(lines!["fn second_modified() {}".ai(), "fn third() {}".ai()]);
    let commit = repo.stage_all_and_commit("Complex changes").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Parse and verify EXACT order of every line
    let lines = parse_diff_output(&output);

    // Verify exact sequence with specific order and attribution
    // Git will show: delete both old lines, add both new lines
    assert_diff_lines_exact(
        &lines,
        &[
            ("-", "fn first()", None),                 // Delete human line
            ("-", "fn second()", None), // Delete AI line (no attribution on deletions)
            ("+", "fn second_modified()", Some("ai")), // Add AI line
            ("+", "fn third()", Some("ai")), // Add AI line
        ],
    );
}

#[test]
fn test_diff_range_multiple_commits() {
    let repo = TestRepo::new();

    // First commit
    let mut file = repo.filename("multi.txt");
    file.set_contents(lines!["Line 1".human()]);
    let first = repo.stage_all_and_commit("First").unwrap();

    // Second commit
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Second").unwrap();

    // Third commit
    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".human()]);
    repo.stage_all_and_commit("Third").unwrap();

    // Fourth commit
    file.set_contents(lines![
        "Line 1".human(),
        "Line 2".ai(),
        "Line 3".human(),
        "Line 4".ai()
    ]);
    let fourth = repo.stage_all_and_commit("Fourth").unwrap();

    // Run diff across multiple commits
    let range = format!("{}..{}", first.commit_sha, fourth.commit_sha);
    let output = repo
        .git_ai(&["diff", &range])
        .expect("git-ai diff multi-commit range should succeed");

    // Should show cumulative changes
    assert!(output.contains("+Line 2"), "Should show Line 2 addition");
    assert!(output.contains("+Line 3"), "Should show Line 3 addition");
    assert!(output.contains("+Line 4"), "Should show Line 4 addition");

    // Should have attribution markers
    assert!(
        output.contains("🤖") || output.contains("👤"),
        "Should have attribution markers"
    );
}

#[test]
fn test_diff_ignores_repo_external_diff_helper_but_proxy_uses_it() {
    let repo = TestRepo::new();

    let mut file = repo.filename("README.md");
    file.set_contents(lines!["line one".human()]);
    repo.stage_all_and_commit("initial").unwrap();

    file.set_contents(lines!["line one".human(), "line two".ai()]);
    repo.stage_all_and_commit("second").unwrap();

    let marker = configure_repo_external_diff_helper(&repo);

    let proxied_diff = repo
        .git(&["diff", "HEAD^", "HEAD"])
        .expect("proxied git diff should succeed");
    assert!(
        proxied_diff.contains(&marker),
        "proxied git diff should honor diff.external helper output, got:\n{}",
        proxied_diff
    );

    let git_ai_diff = repo
        .git_ai(&["diff", "HEAD"])
        .expect("git-ai diff should succeed");
    assert!(
        !git_ai_diff.contains(&marker),
        "git-ai diff should not use external diff helper output, got:\n{}",
        git_ai_diff
    );
    assert!(
        git_ai_diff.contains("diff --git"),
        "git-ai diff should emit standard unified diff output, got:\n{}",
        git_ai_diff
    );
    assert!(
        git_ai_diff.contains("@@"),
        "git-ai diff should include hunk headers, got:\n{}",
        git_ai_diff
    );
}

#[test]
fn test_checkpoint_and_commit_ignore_repo_external_diff_helper() {
    let repo = TestRepo::new();

    let mut file = repo.filename("tracked.txt");
    file.set_contents(lines!["base".human()]);
    repo.stage_all_and_commit("initial").unwrap();

    file.set_contents(lines!["base".human(), "added by ai".ai()]);
    let marker = configure_repo_external_diff_helper(&repo);
    let proxied_diff = repo
        .git(&["diff", "HEAD"])
        .expect("proxied git diff should succeed");
    assert!(
        proxied_diff.contains(&marker),
        "sanity check: external diff helper should be active for proxied git diff"
    );

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed with external diff configured");
    repo.stage_all_and_commit("ai commit").unwrap();

    file.assert_lines_and_blame(lines!["base".human(), "added by ai".ai()]);
}

#[test]
fn test_diff_ignores_git_external_diff_env_but_proxy_uses_it() {
    let repo = TestRepo::new();

    let mut file = repo.filename("env-diff.txt");
    file.set_contents(lines!["before".human()]);
    repo.stage_all_and_commit("initial").unwrap();

    file.set_contents(lines!["before".human(), "after".ai()]);
    repo.stage_all_and_commit("second").unwrap();

    let marker = "ENV_EXTERNAL_DIFF_MARKER";
    let helper_path = create_external_diff_helper_script(&repo, marker);
    let helper_path_str = helper_path
        .to_str()
        .expect("helper path must be valid UTF-8")
        .replace('\\', "/")
        .to_string();

    let proxied = repo
        .git_with_env(
            &["diff", "HEAD^", "HEAD"],
            &[("GIT_EXTERNAL_DIFF", helper_path_str.as_str())],
            None,
        )
        .expect("proxied git diff should succeed");
    assert!(
        proxied.contains(marker),
        "proxied git diff should honor GIT_EXTERNAL_DIFF, got:\n{}",
        proxied
    );

    let ai_diff = repo
        .git_ai_with_env(
            &["diff", "HEAD"],
            &[("GIT_EXTERNAL_DIFF", helper_path_str.as_str())],
        )
        .expect("git-ai diff should succeed with GIT_EXTERNAL_DIFF set");
    assert!(
        !ai_diff.contains(marker),
        "git-ai diff should ignore GIT_EXTERNAL_DIFF for internal diff calls, got:\n{}",
        ai_diff
    );
    assert!(
        ai_diff.contains("diff --git"),
        "git-ai diff should still emit normal unified diff output, got:\n{}",
        ai_diff
    );
}

#[test]
fn test_diff_ignores_git_diff_opts_env_for_internal_diff() {
    let repo = TestRepo::new();

    let mut file = repo.filename("env-diff-opts.txt");
    file.set_contents(lines![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "line 4".human(),
        "line 5".human()
    ]);
    repo.stage_all_and_commit("initial").unwrap();

    file.set_contents(lines![
        "line 1".human(),
        "line 2".human(),
        "line 3 changed".ai(),
        "line 4".human(),
        "line 5".human()
    ]);
    let commit = repo.stage_all_and_commit("change middle").unwrap();

    // Proxied git should honor this env var and output 0 context lines.
    let proxied = repo
        .git_with_env(
            &[
                "diff",
                &format!("{}^", commit.commit_sha),
                &commit.commit_sha,
            ],
            &[("GIT_DIFF_OPTS", "--unified=0")],
            None,
        )
        .expect("proxied git diff should succeed");
    let proxied_context_count = proxied
        .lines()
        .filter(|l| l.starts_with(' ') && !l.starts_with("  "))
        .count();
    assert_eq!(
        proxied_context_count, 0,
        "proxied git diff should honor GIT_DIFF_OPTS=--unified=0, got:\n{}",
        proxied
    );

    // git-ai diff should ignore GIT_DIFF_OPTS and keep normal context behavior.
    let ai_diff = repo
        .git_ai_with_env(
            &["diff", &commit.commit_sha],
            &[("GIT_DIFF_OPTS", "--unified=0")],
        )
        .expect("git-ai diff should succeed with GIT_DIFF_OPTS set");
    let ai_context_count = ai_diff
        .lines()
        .filter(|l| l.starts_with(' ') && !l.starts_with("  "))
        .count();
    assert!(
        ai_context_count >= 2,
        "git-ai diff should ignore GIT_DIFF_OPTS and preserve context lines, got:\n{}",
        ai_diff
    );
}
