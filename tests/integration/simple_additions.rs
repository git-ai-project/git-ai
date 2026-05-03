use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use std::fs;

fn configure_diff_settings(repo: &TestRepo, settings: &[(&str, &str)]) {
    for (key, value) in settings {
        repo.git_og(&["config", key, value])
            .unwrap_or_else(|err| panic!("setting {key}={value} should succeed: {err}"));
    }
}

fn run_simple_additions_with_diff_settings(settings: &[(&str, &str)]) {
    let repo = TestRepo::new();
    configure_diff_settings(&repo, settings);

    let mut file = repo.filename("test.txt");
    file.set_contents(crate::lines!["Base line 1", "Base line 2"]);
    repo.stage_all_and_commit("Base commit").unwrap();

    file.insert_at(
        2,
        crate::lines!["NEW LINEs From Claude!".ai(), "Hello".ai(), "World".ai(),],
    );
    repo.stage_all_and_commit("AI additions").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "Base line 1".human(),
        "Base line 2".ai(),
        "NEW LINEs From Claude!".ai(),
        "Hello".ai(),
        "World".ai(),
    ]);
}

#[test]
fn test_simple_additions_empty_repo() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(crate::lines!["Line1", "Line 2".ai(), "Line 3".ai(),]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    file.assert_lines_and_blame(crate::lines!["Line1".human(), "Line 2".ai(), "Line 3".ai(),]);
}

#[test]
fn test_simple_additions_with_base_commit() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(crate::lines!["Base line 1", "Base line 2"]);

    repo.stage_all_and_commit("Base commit").unwrap();

    file.insert_at(
        2,
        crate::lines!["NEW LINEs From Claude!".ai(), "Hello".ai(), "World".ai(),],
    );

    repo.stage_all_and_commit("AI additions").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "Base line 1".human(),
        "Base line 2".ai(),
        "NEW LINEs From Claude!".ai(),
        "Hello".ai(),
        "World".ai(),
    ]);
}

#[test]
fn test_simple_additions_with_base_commit_and_custom_diff_config() {
    run_simple_additions_with_diff_settings(&[
        ("diff.wordregex", r"\w+|[^[:space:]]+"),
        ("diff.mnemonicprefix", "true"),
        ("diff.renames", "copies"),
        ("diff.noprefix", "true"),
    ]);
}

#[test]
fn test_simple_additions_with_diff_noprefix_enabled() {
    run_simple_additions_with_diff_settings(&[("diff.noprefix", "true")]);
}

#[test]
fn test_simple_additions_with_diff_mnemonicprefix_enabled() {
    run_simple_additions_with_diff_settings(&[("diff.mnemonicprefix", "true")]);
}

#[test]
fn test_simple_additions_with_diff_renames_copies() {
    run_simple_additions_with_diff_settings(&[("diff.renames", "copies")]);
}

#[test]
fn test_simple_additions_with_diff_relative_enabled() {
    run_simple_additions_with_diff_settings(&[("diff.relative", "true")]);
}

#[test]
fn test_simple_additions_with_custom_diff_prefixes() {
    run_simple_additions_with_diff_settings(&[
        ("diff.srcPrefix", "SRC/"),
        ("diff.dstPrefix", "DST/"),
    ]);
}

#[test]
fn test_simple_additions_with_diff_algorithm_histogram() {
    run_simple_additions_with_diff_settings(&[("diff.algorithm", "histogram")]);
}

#[test]
fn test_simple_additions_with_diff_indent_heuristic_disabled() {
    run_simple_additions_with_diff_settings(&[("diff.indentHeuristic", "false")]);
}

#[test]
fn test_simple_additions_with_diff_inter_hunk_context() {
    run_simple_additions_with_diff_settings(&[("diff.interHunkContext", "8")]);
}

#[test]
fn test_simple_additions_with_color_diff_always() {
    run_simple_additions_with_diff_settings(&[("color.diff", "always"), ("color.ui", "always")]);
}

#[test]
fn test_simple_additions_on_top_of_ai_contributions() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(crate::lines!["Line 1", "Line 2", "Line 3"]);

    repo.stage_all_and_commit("Base commit").unwrap();

    file.insert_at(3, crate::lines!["AI Line 1".ai(), "AI Line 2".ai(),]);

    repo.stage_all_and_commit("AI commit").unwrap();

    file.replace_at(3, "HUMAN EDITED AI LINE".human());

    repo.stage_all_and_commit("Human edits AI").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "Line 1".human(),
        "Line 2".human(),
        "Line 3".ai(),
        "HUMAN EDITED AI LINE".human(),
        "AI Line 2".ai(),
    ]);
}

#[test]
fn test_simple_additions_new_file_not_git_added() {
    let repo = TestRepo::new();
    let mut file = repo.filename("new_file.txt");

    // Create a new file with human lines, then add AI lines before any git add
    file.set_contents(crate::lines![
        "Line 1 from human",
        "Line 2 from human",
        "Line 3 from human",
        "Line 4 from AI".ai(),
        "Line 5 from AI".ai(),
    ]);

    let commit = repo.stage_all_and_commit("Initial commit").unwrap();

    // All lines should be attributed correctly
    assert!(!commit.authorship_log.attestations.is_empty());

    file.assert_lines_and_blame(crate::lines![
        "Line 1 from human",
        "Line 2 from human",
        "Line 3 from human",
        "Line 4 from AI".ai(),
        "Line 5 from AI".ai(),
    ]);
}

#[test]
fn test_ai_human_interleaved_line_attribution() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(crate::lines!["Base line"]);

    repo.stage_all_and_commit("Base commit").unwrap();

    file.insert_at(
        1,
        crate::lines!["AI Line 1".ai(), "Human Line 1".human(), "AI Line 2".ai()],
    );

    repo.stage_all_and_commit("Interleaved commit").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "Base line".ai(),
        "AI Line 1".ai(),
        "Human Line 1".ai(),
        "AI Line 2".ai(),
    ]);
}

#[test]
fn test_simple_ai_then_human_deletion() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(crate::lines![
        "Line 1", "Line 2", "Line 3", "Line 4", "Line 5"
    ]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    file.insert_at(5, crate::lines!["AI Line".ai()]);

    repo.stage_all_and_commit("AI adds line").unwrap();

    file.delete_at(5);

    let commit = repo.stage_all_and_commit("Human deletes AI line").unwrap();

    // The authorship log should have no attestations since we only deleted lines
    assert_eq!(commit.authorship_log.attestations.len(), 0);

    file.assert_lines_and_blame(crate::lines![
        "Line 1".human(),
        "Line 2".human(),
        "Line 3".human(),
        "Line 4".human(),
        "Line 5".human(),
    ]);
}

#[test]
fn test_multiple_ai_checkpoints_with_human_deletions() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // Two initial lines: "Base" stays human (not adjacent to AI hunks);
    // "Base2" (last line) gets pulled into the AI hunk and becomes AI.
    file.set_contents(crate::lines!["Base", "Base2"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    file.insert_at(2, crate::lines!["AI1 Line 1".ai(), "AI1 Line 2".ai()]);
    file.insert_at(4, crate::lines!["AI2 Line 1".ai(), "AI2 Line 2".ai()]);

    // Delete the first AI session's lines (indices 2 and 3)
    file.delete_range(2, 4);

    let commit = repo.stage_all_and_commit("Complex commit").unwrap();

    // Should only have AI2's lines attributed (now at indices 2 and 3 after deletion)
    assert_eq!(commit.authorship_log.attestations.len(), 1);

    // "Base" stays human — it's not at the hunk boundary.
    // "Base2" becomes AI — it was the last line in the original, so force_split
    // places it in the same 1→N hunk as the AI insertions.
    file.assert_lines_and_blame(crate::lines![
        "Base".human(),
        "Base2".ai(),
        "AI2 Line 1".ai(),
        "AI2 Line 2".ai(),
    ]);
}

#[test]
fn test_complex_mixed_additions_and_deletions() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(crate::lines![
        "Line 1", "Line 2", "Line 3", "Line 4", "Line 5", "Line 6", "Line 7", "Line 8", "Line 9",
        "Line 10",
    ]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI deletes lines 2-3 and replaces with new content (delete at index 1, 2 items)
    file.delete_range(1, 3);
    file.insert_at(
        1,
        crate::lines!["NEW LINE A".ai(), "NEW LINE B".ai(), "NEW LINE C".ai(),],
    );

    // AI inserts at the end
    file.insert_at(11, crate::lines!["END LINE 1".ai(), "END LINE 2".ai(),]);

    let commit = repo.stage_all_and_commit("Complex edits").unwrap();

    // Should have lines 2-4 and the last 2 lines attributed to AI
    assert_eq!(commit.authorship_log.attestations.len(), 1);

    file.assert_lines_and_blame(crate::lines![
        "Line 1".human(),
        "NEW LINE A".ai(),
        "NEW LINE B".ai(),
        "NEW LINE C".ai(),
        "Line 4".human(),
        "Line 5".human(),
        "Line 6".human(),
        "Line 7".human(),
        "Line 8".human(),
        "Line 9".human(),
        "Line 10".ai(),
        "END LINE 1".ai(),
        "END LINE 2".ai(),
    ]);
}

#[test]
fn test_ai_adds_lines_multiple_commits() {
    // Test AI adding lines across multiple commits
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(crate::lines!["base_line", ""]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    file.insert_at(
        1,
        crate::lines!["ai_line1".ai(), "ai_line2".ai(), "ai_line3".ai(),],
    );

    repo.stage_all_and_commit("AI adds first batch").unwrap();

    file.insert_at(4, crate::lines!["ai_line4".ai(), "ai_line5".ai(),]);

    repo.stage_all_and_commit("AI adds second batch").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "base_line".human(),
        "ai_line1".ai(),
        "ai_line2".ai(),
        "ai_line3".ai(),
        "ai_line4".ai(),
        "ai_line5".ai(),
    ]);
}

#[test]
fn test_partial_staging_filters_unstaged_lines() {
    // Test where AI makes changes but only some are staged
    let repo = TestRepo::new();
    let mut file = repo.filename("partial.ts");

    file.set_contents(crate::lines!["line1", "line2", "line3"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI modifies lines 2-3 and we stage immediately
    file.replace_at(1, "ai_modified2".ai());
    file.replace_at(2, "ai_modified3".ai());

    file.stage();

    // Now AI adds more lines that won't be staged
    file.insert_at(
        3,
        crate::lines!["unstaged_line1".ai(), "unstaged_line2".ai()],
    );

    let commit = repo.commit("Partial staging").unwrap();

    // The commit should only include the modifications, not the unstaged additions
    assert_eq!(commit.authorship_log.attestations.len(), 1);

    // Only check committed lines (unstaged lines will be ignored)
    file.assert_committed_lines(crate::lines![
        "line1".human(),
        "ai_modified2".ai(),
        // ai_modified3 is ai, but it's not considered committed, because adding the subsequent uncommitted lines also added a newline char to this line
    ]);
}

#[test]
fn test_human_stages_some_ai_lines() {
    // Test where AI adds multiple lines but human only stages some of them
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(crate::lines!["line1", "line2", "line3"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds lines 4-8
    file.insert_at(
        3,
        crate::lines![
            "ai_line4".ai(),
            "ai_line5".ai(),
            "ai_line6".ai(),
            "ai_line7".ai(),
            "ai_line8".ai(),
        ],
    );

    file.stage();

    // Human adds an unstaged line
    file.insert_at(8, crate::lines!["human_unstaged".human()]);

    let commit = repo.commit("Partial AI commit").unwrap();
    assert_eq!(commit.authorship_log.attestations.len(), 1);

    // Only check committed lines (unstaged human line will be ignored)
    file.assert_committed_lines(crate::lines![
        "line1".human(),
        "line2".human(),
        "line3".ai(),
        "ai_line4".ai(),
        "ai_line5".ai(),
        "ai_line6".ai(),
        "ai_line7".ai(),
        // ai_line8 is ai, but it's not considered committed, because adding the subsequent uncommitted lines also added a newline char to this line
    ]);
}

#[test]
fn test_multiple_ai_sessions_with_partial_staging() {
    // Multiple AI sessions, but only one has staged changes
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(crate::lines!["line1", "line2", "line3"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // First AI session adds lines and they get staged
    file.insert_at(
        3,
        crate::lines!["ai1_line1".ai(), "ai1_line2".ai(), "ai1_line3".ai()],
    );

    file.stage();

    // Second AI session adds lines but they DON'T get staged
    file.insert_at(
        6,
        crate::lines!["ai2_line1".ai(), "ai2_line2".ai(), "ai2_line3".ai()],
    );

    let commit = repo.commit("Commit first AI session only").unwrap();
    assert_eq!(commit.authorship_log.attestations.len(), 1);

    // Only check committed lines (second AI session unstaged)
    file.assert_committed_lines(crate::lines![
        "line1".human(),
        "line2".human(),
        "line3".ai(),
        "ai1_line1".ai(),
        "ai1_line2".ai(),
        // ai1_line3 is ai, but it's not considered committed, because adding the subsequent uncommitted lines also added a newline char to this line
    ]);
}

#[test]
fn test_ai_adds_then_commits_in_batches() {
    // AI adds lines in multiple batches, committing separately
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(crate::lines!["line1", "line2", "line3", "line4", ""]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds first batch of lines
    file.insert_at(
        4,
        crate::lines!["ai_line5".ai(), "ai_line6".ai(), "ai_line7".ai()],
    );
    file.stage();

    repo.commit("Add lines 5-7").unwrap();

    // AI adds second batch of lines
    file.insert_at(
        7,
        crate::lines!["ai_line8".ai(), "ai_line9".ai(), "ai_line10".ai()],
    );

    repo.stage_all_and_commit("Add lines 8-10").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "line1".human(),
        "line2".human(),
        "line3".human(),
        "line4".human(),
        "ai_line5".ai(),
        "ai_line6".ai(),
        "ai_line7".ai(),
        "ai_line8".ai(),
        "ai_line9".ai(),
        "ai_line10".ai(),
    ]);
}

#[test]
fn test_ai_edits_with_partial_staging() {
    // AI makes modifications, some staged and some not
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(crate::lines!["line1", "line2", "line3", "line4", "line5"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI modifies some lines
    file.replace_at(1, "ai_modified_line2".ai());
    file.replace_at(3, "ai_modified_line4".ai());

    // Stage only the modifications
    file.stage();

    // AI adds more lines that won't be staged
    file.insert_at(
        5,
        crate::lines!["ai_line6".ai(), "ai_line7".ai(), "ai_line8".ai()],
    );

    let commit = repo.commit("Partial staging").unwrap();

    // With per-trace attestation keys, we may have multiple entries per file
    assert!(!commit.authorship_log.attestations.is_empty());

    // Only check committed lines
    file.assert_committed_lines(crate::lines![
        "line1".human(),
        "ai_modified_line2".ai(),
        "line3".human(),
        "ai_modified_line4".ai(),
        // line5 is human, but it's not considered committed, because adding line 6+ also added a newline char to line 5
    ]);
}

#[test]
fn test_unstaged_changes_not_committed() {
    // Test that unstaged changes don't appear in the commit
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(crate::lines!["line1", "line2", "line3"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds lines at the end and stages them
    file.insert_at(3, crate::lines!["ai_line4".ai(), "ai_line5".ai()]);
    file.stage();

    // AI adds more lines that won't be staged
    file.insert_at(
        5,
        crate::lines!["unstaged_line6".ai(), "unstaged_line7".ai()],
    );

    let commit = repo.commit("Commit only staged lines").unwrap();

    // Only the staged lines should be in the commit
    assert!(!commit.authorship_log.attestations.is_empty());

    // Only check committed lines
    file.assert_committed_lines(crate::lines![
        "line1".human(),
        "line2".human(),
        "line3".ai(),
        "ai_line4".ai(),
        // line 5 is ai, but it's not considered committed, because adding line 6+ also added a newline char to line 5
    ]);
}

#[test]
fn test_unstaged_ai_lines_saved_to_working_log() {
    // Test that unstaged AI-authored lines are saved to the working log for the next commit
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(crate::lines!["line1", "line2", "line3", ""]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds lines 4-7 and stages some
    file.insert_at(3, crate::lines!["ai_line4".ai(), "ai_line5".ai()]);
    file.stage();

    // AI adds more lines that won't be staged
    file.insert_at(5, crate::lines!["ai_line6".ai(), "ai_line7".ai()]);

    // Commit only the staged lines
    let first_commit = repo.commit("Partial AI commit").unwrap();

    // The commit should only have lines 4-5
    assert_eq!(first_commit.authorship_log.attestations.len(), 1);

    // Now stage and commit the remaining lines
    file.stage();
    let second_commit = repo.commit("Commit remaining AI lines").unwrap();

    // The second commit should also attribute lines 6-7 to AI
    assert_eq!(second_commit.authorship_log.attestations.len(), 1);

    // Final state should have all AI lines attributed
    file.assert_lines_and_blame(crate::lines![
        "line1".human(),
        "line2".human(),
        "line3".human(),
        "ai_line4".ai(),
        "ai_line5".ai(),
        "ai_line6".ai(),
        "ai_line7".ai(),
    ]);
}

/// Test: New file with partial staging across two commits
/// AI creates a new file with many lines, stage only some, then commit the rest
#[test]
fn test_new_file_partial_staging_two_commits() {
    let repo = TestRepo::new();

    // Create an initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a brand new file with planets
    let mut file = repo.filename("planets.txt");
    file.set_contents(crate::lines![
        "Mercury".ai(),
        "Venus".ai(),
        "Earth".ai(),
        "Mars".ai(),
        "Jupiter".ai(),
        "Saturn".ai(),
        "Uranus".ai(),
        "Neptune".ai(),
        "Pluto (dwarf)".ai(),
    ]);

    // First commit should have all the planets
    let first_commit = repo.stage_all_and_commit("Add planets").unwrap();

    assert_eq!(first_commit.authorship_log.attestations.len(), 1);

    file.assert_lines_and_blame(crate::lines![
        "Mercury".ai(),
        "Venus".ai(),
        "Earth".ai(),
        "Mars".ai(),
        "Jupiter".ai(),
        "Saturn".ai(),
        "Uranus".ai(),
        "Neptune".ai(),
        "Pluto (dwarf)".ai(),
    ]);
}

#[test]
fn test_checkpoint_then_stage_then_checkpoint_across_two_commits_preserves_ai_lines() {
    // Exact reproduction from bug report.
    let repo = TestRepo::new();
    let file_path = repo.path().join("example.txt");

    fs::write(&file_path, "test\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "example.txt"])
        .unwrap();

    repo.git(&["add", "."]).unwrap();

    fs::write(&file_path, "test\ntest1\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "example.txt"])
        .unwrap();

    let first_commit = repo.commit("test").unwrap();
    assert!(
        !first_commit.authorship_log.attestations.is_empty(),
        "first commit should include AI attribution for line 1"
    );

    let mut file = repo.filename("example.txt");
    file.assert_committed_lines(lines!["test".ai()]);

    repo.git(&["add", "."]).unwrap();
    let second_commit = repo.commit("test1").unwrap();
    assert!(
        !second_commit.authorship_log.attestations.is_empty(),
        "second commit should include AI attribution for line 2"
    );

    file.assert_lines_and_blame(lines!["test".ai(), "test1".ai()]);
}

#[test]
fn test_checkpoint_stage_checkpoint_with_non_adjacent_hunks_preserves_second_hunk_ai() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("example.md");

    let initial = "\
# Notes
intro line

**Section Alpha**
alpha 1
alpha 2
alpha 3

middle context
another context
yet another context

**Section Omega**
omega 1
omega 2
omega 3
";
    fs::write(&file_path, initial).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let first_ai_hunk_only = "\
# Notes
intro line

### Section Alpha
alpha 1
alpha 2
alpha 3

middle context
another context
yet another context

**Section Omega**
omega 1
omega 2
omega 3
";
    fs::write(&file_path, first_ai_hunk_only).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "example.md"])
        .unwrap();

    repo.git(&["add", "."]).unwrap();

    let both_ai_hunks = "\
# Notes
intro line

### Section Alpha
alpha 1
alpha 2
alpha 3

middle context
another context
yet another context

### Section Omega
omega 1
omega 2
omega 3
";
    fs::write(&file_path, both_ai_hunks).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "example.md"])
        .unwrap();

    let first_commit = repo.commit("Commit first staged hunk").unwrap();
    assert!(
        !first_commit.authorship_log.attestations.is_empty(),
        "first commit should include AI attribution for the first hunk"
    );

    let mut file = repo.filename("example.md");
    file.assert_committed_lines(lines![
        "# Notes".human(),
        "intro line".human(),
        "".human(),
        "### Section Alpha".ai(),
        "alpha 1".human(),
        "alpha 2".human(),
        "alpha 3".human(),
        "".human(),
        "middle context".human(),
        "another context".human(),
        "yet another context".human(),
        "".human(),
        "omega 1".human(),
        "omega 2".human(),
        "omega 3".human(),
    ]);

    repo.git(&["add", "."]).unwrap();
    let second_commit = repo.commit("Commit second unstaged hunk").unwrap();
    assert!(
        !second_commit.authorship_log.attestations.is_empty(),
        "second commit should include AI attribution for the second hunk"
    );

    file.assert_lines_and_blame(lines![
        "# Notes".human(),
        "intro line".human(),
        "".human(),
        "### Section Alpha".ai(),
        "alpha 1".human(),
        "alpha 2".human(),
        "alpha 3".human(),
        "".human(),
        "middle context".human(),
        "another context".human(),
        "yet another context".human(),
        "".human(),
        "### Section Omega".ai(),
        "omega 1".human(),
        "omega 2".human(),
        "omega 3".human(),
    ]);
}

#[test]
fn test_using_test_repo_with_custom_checkpoints() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("example.md");

    let initial = "\
Untracked line
";
    fs::write(&file_path, initial).unwrap();
    // Example of a completely untracked edit where we didn't fire a checkpoint call at all
    repo.stage_all_and_commit("Initial commit").unwrap();
    // Assert after every commit
    let mut file = repo.filename("example.md");
    // ALWAYS use the helper to assert the lines post-commit AND make sure to always assert line-level after EVERY commit for EVERY test you EVER right. This is CRUCIAL.
    file.assert_committed_lines(lines![
        "Untracked line".unattributed_human(), // 'untracked'
    ]);

    let second_edit = "\
Untracked line
Human line
";
    fs::write(&file_path, second_edit).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "example.md"])
        .unwrap();

    // Explicit add call (very useful to test partial staging scenarios)
    repo.git(&["add", "."]).unwrap();
    // Explicit commit
    repo.commit("Second commit").unwrap();
    file.assert_committed_lines(lines![
        "Untracked line".unattributed_human(), // still 'untracked'
        "Human line".human(),                  // known human
    ]);

    let third_edit = "\
Untracked line
Human line
AI line
";
    fs::write(&file_path, third_edit).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "example.md"])
        .unwrap();
    // Example of a completely untracked edit where we didn't fire a checkpoint call at all
    repo.stage_all_and_commit("Third commit").unwrap();
    file.assert_committed_lines(lines![
        "Untracked line".unattributed_human(), // 'untracked'
        "Human line".human(),                  // known human
        "AI line".ai(),                        // AI line
    ]);

    let fourth_edit = "\
Untracked line
Human line
AI line
Another untracked line
";
    fs::write(&file_path, fourth_edit).unwrap();
    // Mocking an AI agent preset's pre edit checkpoint, which all the AI agent presets do to exclude
    // changes made by something else (impossible to know what) before the AI makes its own edit. We mock
    // that by calling a 'legacy human' (untracked) checkpoint.
    repo.git_ai(&["checkpoint", "human", "example.md"]).unwrap();

    let fifth_edit = "\
Untracked line
Human line
AI line
Another untracked line
Another AI line
";
    fs::write(&file_path, fifth_edit).unwrap();
    // Mocking an AI agent preset's post edit checkpoint, which all the AI agent presets do to capture the changes made by the AI.
    // We mock that by calling a 'mock_ai' checkpoint.
    repo.git_ai(&["checkpoint", "mock_ai", "example.md"])
        .unwrap();
    repo.stage_all_and_commit("Fourth commit").unwrap();
    file.assert_committed_lines(lines![
        "Untracked line".unattributed_human(),         // 'untracked'
        "Human line".human(),                          // known human
        "AI line".ai(),                                // AI line
        "Another untracked line".unattributed_human(), // 'untracked'
        "Another AI line".ai(),                        // AI line
    ]);
}

#[test]
fn test_ai_heading_checkpoint_then_human_top_commit_then_rest_preserves_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("aidanwashere.md");

    let initial = "\
> \"First, solve the problem. Then, write the code.\"
> \"It works on my machine.\"

*Verse 1:*
Aidan was here, left his mark on the page,
Writing code through the night, line by line, stage by stage.

*Chorus:*
Oh, Aidan was here, yeah, Aidan was here,
The git log will show it, the history's clear.

*Verse 2:*
From branches to merges, through conflicts and fear,
One thing is certain - Aidan was here.
";
    fs::write(&file_path, initial).unwrap();
    repo.stage_all_and_commit("Initial markdown").unwrap();

    let ai_rewrites = "\
> \"First, solve the problem. Then, write the code.\"
> \"It works on my machine.\"

### Verse 1
Aidan was here, left his mark on the page,
Writing code through the night, line by line, stage by stage.

### Chorus
Oh, Aidan was here, yeah, Aidan was here,
The git log will show it, the history's clear.

### Verse 2
From branches to merges, through conflicts and fear,
One thing is certain - Aidan was here.
";
    fs::write(&file_path, ai_rewrites).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "aidanwashere.md"])
        .unwrap();

    let with_human_top = "\
Human preface 1
Human preface 2

> \"First, solve the problem. Then, write the code.\"
> \"It works on my machine.\"

### Verse 1
Aidan was here, left his mark on the page,
Writing code through the night, line by line, stage by stage.

### Chorus
Oh, Aidan was here, yeah, Aidan was here,
The git log will show it, the history's clear.

### Verse 2
From branches to merges, through conflicts and fear,
One thing is certain - Aidan was here.
";
    fs::write(&file_path, with_human_top).unwrap();
    // Intentionally no checkpoint for this human top edit.

    let patch_path = repo.path().join(".git").join("stage_human_top_only.patch");
    let top_hunk_patch = "\
diff --git a/aidanwashere.md b/aidanwashere.md
--- a/aidanwashere.md
+++ b/aidanwashere.md
@@ -0,0 +1,3 @@
+Human preface 1
+Human preface 2
+
";
    fs::write(&patch_path, top_hunk_patch).unwrap();
    repo.git(&[
        "apply",
        "--cached",
        "--unidiff-zero",
        patch_path.to_str().unwrap(),
    ])
    .unwrap();

    let first_commit = repo.commit("Commit human top section").unwrap();
    assert_eq!(
        first_commit.authorship_log.attestations.len(),
        0,
        "first commit should only contain human top insertion"
    );

    repo.git(&["add", "."]).unwrap();
    let second_commit = repo.commit("Commit remaining heading rewrites").unwrap();
    assert!(
        !second_commit.authorship_log.attestations.is_empty(),
        "second commit should contain AI heading rewrite attributions"
    );

    let mut file = repo.filename("aidanwashere.md");
    file.assert_lines_and_blame(lines![
        "Human preface 1".human(),
        "Human preface 2".human(),
        "".human(),
        "> \"First, solve the problem. Then, write the code.\"".human(),
        "> \"It works on my machine.\"".human(),
        "".human(),
        "### Verse 1".ai(),
        "Aidan was here, left his mark on the page,".human(),
        "Writing code through the night, line by line, stage by stage.".human(),
        "".human(),
        "### Chorus".ai(),
        "Oh, Aidan was here, yeah, Aidan was here,".human(),
        "The git log will show it, the history's clear.".human(),
        "".human(),
        "### Verse 2".ai(),
        "From branches to merges, through conflicts and fear,".human(),
        "One thing is certain - Aidan was here.".human(),
    ]);
}

#[test]
fn test_mock_ai_with_pathspecs() {
    let repo = TestRepo::new();
    let mut file1 = repo.filename("file1.txt");
    let mut file2 = repo.filename("file2.txt");

    // Create initial state
    file1.set_contents(crate::lines!["File1 Line 1", "File1 Line 2"]);
    file2.set_contents(crate::lines!["File2 Line 1", "File2 Line 2"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make changes to both files
    file1.insert_at(2, crate::lines!["File1 AI Line".ai()]);
    file2.insert_at(2, crate::lines!["File2 Human Line"]);

    // Use mock_ai with pathspec to only checkpoint file1.txt
    repo.git_ai(&["checkpoint", "mock_ai", "file1.txt"])
        .unwrap();

    // Commit the changes
    repo.stage_all_and_commit("Second commit").unwrap();

    // file1 should have AI attribution for the new line
    file1.assert_lines_and_blame(crate::lines![
        "File1 Line 1".human(),
        "File1 Line 2".ai(),
        "File1 AI Line".ai(),
    ]);

    // file2 should be all human since we didn't checkpoint it with mock_ai
    file2.assert_lines_and_blame(crate::lines![
        "File2 Line 1".human(),
        "File2 Line 2".human(),
        "File2 Human Line".human(),
    ]);
}

#[test]
fn test_with_duplicate_lines() {
    // This test verifies that squash merge correctly preserves AI authorship for duplicate lines
    let repo = TestRepo::new();
    let mut file = repo.filename("helpers.rs");

    // Create master branch with first function (human-authored)
    file.set_contents(crate::lines![
        "pub fn format_string(s: &str) -> String {",
        "    s.to_uppercase()",
        "}",
    ]);
    repo.stage_all_and_commit("Add format_string function")
        .unwrap();

    file = repo.filename("helpers.rs");
    file.assert_lines_and_blame(crate::lines![
        "pub fn format_string(s: &str) -> String {".human(),
        "    s.to_uppercase()".human(),
        "}".human(),
    ]);

    // AI adds a second function
    // The key test: the second `}` on line 6 is AI-authored, but there's already a `}` on line 3
    let file_path = repo.path().join("helpers.rs");
    fs::write(
        &file_path,
        "pub fn format_string(s: &str) -> String {\n    s.to_uppercase()\n}\npub fn reverse_string(s: &str) -> String {\n    s.chars().rev().collect()\n}",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    repo.stage_all_and_commit("AI adds reverse_string function")
        .unwrap();

    file = repo.filename("helpers.rs");
    file.assert_lines_and_blame(crate::lines![
        "pub fn format_string(s: &str) -> String {".human(),
        "    s.to_uppercase()".human(),
        "}".ai(), // This is the attribution for the AI closing brace. Not natural, but this is how git works!
        "pub fn reverse_string(s: &str) -> String {".ai(),
        "    s.chars().rev().collect()".ai(),
        "}".human(), // Is human, because of how git diffs work!
    ]);
}

#[test]
fn test_ai_deletion_with_human_checkpoint_in_same_commit() {
    // Regression test for issue #193
    // When both human and AI checkpoints happen in the same commit,
    // and AI deletes its own lines, human additions should still be
    // attributed correctly (not claimed by AI)
    use std::fs;

    let repo = TestRepo::new();
    let file_path = repo.path().join("data.txt");

    fs::write(&file_path, "Base Line 1\nBase Line 2\nBase Line 3").unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();

    fs::write(
        &file_path,
        "Base Line 1\nBase Line 2\nAI: Line 1\nAI: Line 2\nAI: Line 3\nBase Line 3",
    )
    .unwrap();

    // Mark only the AI lines with mock_ai checkpoint
    repo.git_ai(&["checkpoint", "mock_ai", "data.txt"]).unwrap();

    repo.stage_all_and_commit("Commit 1: AI adds 3 lines")
        .unwrap();

    // COMMIT 2: Human adds 2 lines, then AI modifies
    // -------
    // Step 1: Human adds lines
    fs::write(
        &file_path,
        "Base Line 1\nBase Line 2\nAI: Line 1\nAI: Line 2\nAI: Line 3\nHuman: Line 1\nHuman: Line 2\nBase Line 3",
    )
    .unwrap();

    // KnownHuman checkpoint for the human-added lines
    repo.git_ai(&["checkpoint", "mock_known_human", "data.txt"])
        .unwrap();

    // Step 2: AI deletes one of its own lines and adds 2 new lines
    fs::write(
        &file_path,
        "Base Line 1\nBase Line 2\nAI: Line 1\nAI: Line 3\nHuman: Line 1\nHuman: Line 2\nAI: New Line 1\nAI: New Line 2\nBase Line 3",
    )
    .unwrap();

    // AI checkpoint
    println!(
        "checkpoint: {}",
        repo.git_ai(&["checkpoint", "mock_ai", "data.txt"]).unwrap()
    );

    // Now commit everything together
    let commit = repo
        .stage_all_and_commit("Commit 2: Human adds 2, AI deletes 1 and adds 2")
        .unwrap();

    commit.print_authorship();

    println!("file: {:?}", repo.git_ai(&["blame", "data.txt"]).unwrap());

    // Verify line-by-line attribution
    let mut file = repo.filename("data.txt");
    file.assert_lines_and_blame(crate::lines![
        "Base Line 1".human(),
        "Base Line 2".human(),
        "AI: Line 1".ai(),
        "AI: Line 3".ai(),
        "Human: Line 1".human(), // Should be human, not AI (Bug #193)
        "Human: Line 2".human(), // Should be human, not AI (Bug #193)
        "AI: New Line 1".ai(),
        "AI: New Line 2".ai(),
        "Base Line 3".human(),
    ]);

    // Verify the stats are correct for the last commit
    let stats_output = repo.git_ai(&["stats", "HEAD", "--json"]).unwrap();
    let stats_output = stats_output.split("}}}").next().unwrap().to_string() + "}}}";
    let stats: serde_json::Value = serde_json::from_str(&stats_output).unwrap();

    // Expected: 2 human additions, 2 AI additions
    // Bug #193 causes: 0 human additions, 4 AI additions
    assert_eq!(
        stats["human_additions"].as_u64().unwrap(),
        2,
        "Human additions should be 2, not 0 (Bug #193)"
    );
    assert_eq!(
        stats["ai_additions"].as_u64().unwrap(),
        2,
        "AI additions should be 2, not 4 (Bug #193)"
    );
}

#[test]
fn test_large_ai_readme_rewrite_with_no_data_bug() {
    // Regression test for bug where AI-authored lines show [no-data]
    // This replicates the exact scenario from commit a630f58cb9b1943cba895a38d00c4c4ed727e37c
    use std::fs;

    let repo = TestRepo::new();
    eprintln!("repo path: {:}", repo.path().to_str().unwrap());
    let file_path = repo.path().join("Readme.md");

    // First commit: Initial human content (exact content from the diff)
    fs::write(
        &file_path,
        "## A quick demo of Git AI Rewrites\n\ndasdas\n\nHUMAN",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial README").unwrap();

    // Second commit: AI completely rewrites the README (exact content from the diff)
    fs::write(
        &file_path,
        "# Set Operations Library

A TypeScript library providing essential set operations for working with JavaScript `Set` objects. This library offers a collection of utility functions for performing common set operations like union, intersection, difference, and more.

## Features

This library provides the following set operations:

- **Union** - Combine all elements from two sets
- **Intersection** - Find elements common to both sets
- **Difference** - Find elements in the first set but not in the second
- **Symmetric Difference** - Find elements in either set but not in both
- **Superset Check** - Determine if one set contains all elements of another
- **Subset Check** - Determine if one set is contained within another

## Installation

Since this is a TypeScript project, you can use the functions directly by importing them:

```typescript
import { union, intersection, difference } from './set-ops';
// or
import { setUnion, setIntersect, setDiff } from './src/set-ops';
```

## Usage

### Basic Operations

```typescript
import { union, intersection, difference, symmetricDifference } from './set-ops';

// Create some sets
const setA = new Set([1, 2, 3, 4]);
const setB = new Set([3, 4, 5, 6]);

// Union: all elements from both sets
const unionResult = union(setA, setB);
// Result: Set { 1, 2, 3, 4, 5, 6 }

// Intersection: elements in both sets
const intersectionResult = intersection(setA, setB);
// Result: Set { 3, 4 }

// Difference: elements in setA but not in setB
const differenceResult = difference(setA, setB);
// Result: Set { 1, 2 }

// Symmetric Difference: elements in either set but not both
const symDiffResult = symmetricDifference(setA, setB);
// Result: Set { 1, 2, 5, 6 }
```

### Set Relationships

```typescript
import { isSuperset, isSubset } from './set-ops';

const setA = new Set([1, 2, 3, 4, 5]);
const setB = new Set([2, 3, 4]);

// Check if setA is a superset of setB
const isSuper = isSuperset(setA, setB);
// Result: true

// Check if setB is a subset of setA
const isSub = isSubset(setB, setA);
// Result: true
```

### Working with Different Types

All functions are generic and work with any type:

```typescript
// Strings
const fruitsA = new Set(['apple', 'banana', 'orange']);
const fruitsB = new Set(['banana', 'grape', 'apple']);
const allFruits = union(fruitsA, fruitsB);

// Objects (with proper comparison)
const usersA = new Set([{ id: 1 }, { id: 2 }]);
const usersB = new Set([{ id: 2 }, { id: 3 }]);
const allUsers = union(usersA, usersB);
```

## API Reference

### `union<T>(setA: Set<T>, setB: Set<T>): Set<T>`

Returns a new set containing all elements from both `setA` and `setB`.

### `intersection<T>(setA: Set<T>, setB: Set<T>): Set<T>`

Returns a new set containing only the elements that are present in both `setA` and `setB`.

### `difference<T>(setA: Set<T>, setB: Set<T>): Set<T>`

Returns a new set containing elements that are in `setA` but not in `setB`.

### `symmetricDifference<T>(setA: Set<T>, setB: Set<T>): Set<T>`

Returns a new set containing elements that are in either `setA` or `setB`, but not in both.

### `isSuperset<T>(set: Set<T>, subset: Set<T>): boolean`

Returns `true` if `set` contains all elements of `subset`, `false` otherwise.

### `isSubset<T>(set: Set<T>, superset: Set<T>): boolean`

Returns `true` if all elements of `set` are contained in `superset`, `false` otherwise.

## Notes

- All functions return new `Set` objects and do not modify the input sets
- Functions are generic and work with any type `T`
- Empty sets are handled correctly in all operations

## License

This project is open source and available for use.
"
    )
    .unwrap();

    // Mark the AI-authored content with mock_ai checkpoint
    repo.git_ai(&["checkpoint", "mock_ai", "Readme.md"])
        .unwrap();

    let commit = repo
        .stage_all_and_commit("AI rewrites README with set operations docs")
        .unwrap();

    // Verify that the commit has AI attestations
    assert_eq!(
        commit.authorship_log.attestations.len(),
        1,
        "Should have exactly one AI attestation"
    );

    // Verify line-by-line attribution for ALL lines
    let mut file = repo.filename("Readme.md");
    file.assert_lines_and_blame(crate::lines![
        "# Set Operations Library".ai(),
        "".human(),
        "A TypeScript library providing essential set operations for working with JavaScript `Set` objects. This library offers a collection of utility functions for performing common set operations like union, intersection, difference, and more.".ai(),
        "".human(),
        "## Features".ai(),
        "".ai(),
        "This library provides the following set operations:".ai(),
        "".ai(),
        "- **Union** - Combine all elements from two sets".ai(),
        "- **Intersection** - Find elements common to both sets".ai(),
        "- **Difference** - Find elements in the first set but not in the second".ai(),
        "- **Symmetric Difference** - Find elements in either set but not in both".ai(),
        "- **Superset Check** - Determine if one set contains all elements of another".ai(),
        "- **Subset Check** - Determine if one set is contained within another".ai(),
        "".ai(),
        "## Installation".ai(),
        "".ai(),
        "Since this is a TypeScript project, you can use the functions directly by importing them:".ai(),
        "".ai(),
        "```typescript".ai(),
        "import { union, intersection, difference } from './set-ops';".ai(),
        "// or".ai(),
        "import { setUnion, setIntersect, setDiff } from './src/set-ops';".ai(),
        "```".ai(),
        "".ai(),
        "## Usage".ai(),
        "".ai(),
        "### Basic Operations".ai(),
        "".ai(),
        "```typescript".ai(),
        "import { union, intersection, difference, symmetricDifference } from './set-ops';".ai(),
        "".ai(),
        "// Create some sets".ai(),
        "const setA = new Set([1, 2, 3, 4]);".ai(),
        "const setB = new Set([3, 4, 5, 6]);".ai(),
        "".ai(),
        "// Union: all elements from both sets".ai(),
        "const unionResult = union(setA, setB);".ai(),
        "// Result: Set { 1, 2, 3, 4, 5, 6 }".ai(),
        "".ai(),
        "// Intersection: elements in both sets".ai(),
        "const intersectionResult = intersection(setA, setB);".ai(),
        "// Result: Set { 3, 4 }".ai(),
        "".ai(),
        "// Difference: elements in setA but not in setB".ai(),
        "const differenceResult = difference(setA, setB);".ai(),
        "// Result: Set { 1, 2 }".ai(),
        "".ai(),
        "// Symmetric Difference: elements in either set but not both".ai(),
        "const symDiffResult = symmetricDifference(setA, setB);".ai(),
        "// Result: Set { 1, 2, 5, 6 }".ai(),
        "```".ai(),
        "".ai(),
        "### Set Relationships".ai(),
        "".ai(),
        "```typescript".ai(),
        "import { isSuperset, isSubset } from './set-ops';".ai(),
        "".ai(),
        "const setA = new Set([1, 2, 3, 4, 5]);".ai(),
        "const setB = new Set([2, 3, 4]);".ai(),
        "".ai(),
        "// Check if setA is a superset of setB".ai(),
        "const isSuper = isSuperset(setA, setB);".ai(),
        "// Result: true".ai(),
        "".ai(),
        "// Check if setB is a subset of setA".ai(),
        "const isSub = isSubset(setB, setA);".ai(),
        "// Result: true".ai(),
        "```".ai(),
        "".ai(),
        "### Working with Different Types".ai(),
        "".ai(),
        "All functions are generic and work with any type:".ai(),
        "".ai(),
        "```typescript".ai(),
        "// Strings".ai(),
        "const fruitsA = new Set(['apple', 'banana', 'orange']);".ai(),
        "const fruitsB = new Set(['banana', 'grape', 'apple']);".ai(),
        "const allFruits = union(fruitsA, fruitsB);".ai(),
        "".ai(),
        "// Objects (with proper comparison)".ai(),
        "const usersA = new Set([{ id: 1 }, { id: 2 }]);".ai(),
        "const usersB = new Set([{ id: 2 }, { id: 3 }]);".ai(),
        "const allUsers = union(usersA, usersB);".ai(),
        "```".ai(),
        "".ai(),
        "## API Reference".ai(),
        "".ai(),
        "### `union<T>(setA: Set<T>, setB: Set<T>): Set<T>`".ai(),
        "".ai(),
        "Returns a new set containing all elements from both `setA` and `setB`.".ai(),
        "".ai(),
        "### `intersection<T>(setA: Set<T>, setB: Set<T>): Set<T>`".ai(),
        "".ai(),
        "Returns a new set containing only the elements that are present in both `setA` and `setB`.".ai(),
        "".ai(),
        "### `difference<T>(setA: Set<T>, setB: Set<T>): Set<T>`".ai(),
        "".ai(),
        "Returns a new set containing elements that are in `setA` but not in `setB`.".ai(),
        "".ai(),
        "### `symmetricDifference<T>(setA: Set<T>, setB: Set<T>): Set<T>`".ai(),
        "".ai(),
        "Returns a new set containing elements that are in either `setA` or `setB`, but not in both.".ai(),
        "".ai(),
        "### `isSuperset<T>(set: Set<T>, subset: Set<T>): boolean`".ai(),
        "".ai(),
        "Returns `true` if `set` contains all elements of `subset`, `false` otherwise.".ai(),
        "".ai(),
        "### `isSubset<T>(set: Set<T>, superset: Set<T>): boolean`".ai(),
        "".ai(),
        "Returns `true` if all elements of `set` are contained in `superset`, `false` otherwise.".ai(),
        "".ai(),
        "## Notes".ai(),
        "".ai(),
        "- All functions return new `Set` objects and do not modify the input sets".ai(),
        "- Functions are generic and work with any type `T`".ai(),
        "- Empty sets are handled correctly in all operations".ai(),
        "".ai(),
        "## License".ai(),
        "".ai(),
        "This project is open source and available for use.".ai(),
    ]);
}

#[test]
fn test_deletion_within_a_single_line_attribution() {
    // Regression test for bug where removing a constructor parameter
    // doesn't get attributed to AI when using mock_ai checkpoint
    // This replicates the scenario where:
    // - constructor(_config: Config, enabled: boolean = true) { [no-data]
    // + constructor(enabled: boolean = true) { [no-data]
    // The constructor line should be attributed to AI
    use std::fs;

    let repo = TestRepo::new();
    let file_path = repo.path().join("git-ai-integration-service.ts");

    // Initial commit: File with old constructor signature (all human)
    fs::write(
        &file_path,
        "/**\n * Service for integrating git-ai hooks into the hook system.\n */\nexport class GitAiIntegrationService {\n  private readonly commandPath: string;\n  private registered = false;\n\n  constructor(_config: Config, enabled: boolean = true) {\n    this.enabled = enabled;\n    this.commandPath = 'git-ai';\n  }\n}\n",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial commit with old constructor")
        .unwrap();

    // Second commit: AI removes the _config parameter
    fs::write(
        &file_path,
        "/**\n * Service for integrating git-ai hooks into the hook system.\n */\nexport class GitAiIntegrationService {\n  private readonly commandPath: string;\n  private registered = false;\n\n  constructor(enabled: boolean = true) {\n    this.enabled = enabled;\n    this.commandPath = 'git-ai';\n  }\n}\n",
    )
    .unwrap();

    // Mark the change as AI-authored
    repo.git_ai(&["checkpoint", "mock_ai", "git-ai-integration-service.ts"])
        .unwrap();

    repo.stage_all_and_commit("AI removes constructor parameter")
        .unwrap();

    // Verify line-by-line attribution - the constructor line should be AI
    let mut file = repo.filename("git-ai-integration-service.ts");
    file.assert_lines_and_blame(crate::lines![
        "/**".human(),
        " * Service for integrating git-ai hooks into the hook system.".human(),
        " */".human(),
        "export class GitAiIntegrationService {".human(),
        "  private readonly commandPath: string;".human(),
        "  private registered = false;".human(),
        "".human(),
        "  constructor(enabled: boolean = true) {".ai(), // Should be AI, not [no-data]
        "    this.enabled = enabled;".human(),
        "    this.commandPath = 'git-ai';".human(),
        "  }".human(),
        "}".human(),
    ]);
}

#[test]
fn test_deletion_of_multiple_lines_by_ai() {
    // Regression test for bug where removing a constructor parameter
    // doesn't get attributed to AI when using mock_ai checkpoint
    // This replicates the scenario where:
    // - constructor(_config: Config, enabled: boolean = true) { [no-data]
    // + constructor(enabled: boolean = true) { [no-data]
    // The constructor line should be attributed to AI
    use std::fs;

    let repo = TestRepo::new();
    let file_path = repo.path().join("git-ai-integration-service.ts");

    // Initial commit: File with old constructor signature (all human)
    fs::write(
        &file_path,
        "/**\n * Service for integrating git-ai hooks into the hook system.\n */\nexport class GitAiIntegrationService {\n  private readonly commandPath: string;\n  private registered = false;\n\n  constructor(_config: Config, enabled: boolean = true) {\n    this.enabled = enabled;\n    this.commandPath = 'git-ai';\n  }\n}\n",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial commit with old constructor")
        .unwrap();

    // Second commit: AI removes the _config parameter
    fs::write(
        &file_path,
        "/**\n * Service for integrating git-ai hooks into the hook system.\n */\nexport class GitAiIntegrationService {\n  private readonly commandPath: string;\n  constructor(_config: Config, enabled: boolean = true) {\n    this.commandPath = 'git-ai';\n  }\n}\n",
    )
    .unwrap();

    // Mark the change as AI-authored
    repo.git_ai(&["checkpoint", "mock_ai", "git-ai-integration-service.ts"])
        .unwrap();

    repo.stage_all_and_commit("AI removes constructor parameter")
        .unwrap();

    // Verify line-by-line attribution - the constructor line should be AI
    let mut file = repo.filename("git-ai-integration-service.ts");
    file.assert_lines_and_blame(crate::lines![
        "/**".human(),
        " * Service for integrating git-ai hooks into the hook system.".human(),
        " */".human(),
        "export class GitAiIntegrationService {".human(),
        "  private readonly commandPath: string;".human(),
        // "  private registered = false;".human(),
        // "".human(),
        "  constructor(_config: Config, enabled: boolean = true) {".human(),
        // "    this.enabled = enabled;".human(),
        "    this.commandPath = 'git-ai';".human(),
        "  }".human(),
        "}".human(),
    ]);
}

/// Regression test for issue #356
/// When AI edits multiple files in the same session, but they are committed
/// in separate batches, the second batch loses AI attribution.
/// See: https://github.com/git-ai-project/git-ai/issues/356
#[test]
fn test_multi_file_batch_commits_preserve_attribution() {
    // This test reproduces the exact scenario from issue #356:
    // 1. AI edits two files (file_a.txt and file_b.txt)
    // 2. User commits file_a.txt first -> AI attribution correct ✓
    // 3. User commits file_b.txt second -> AI attribution should be preserved
    use std::fs;

    let repo = TestRepo::new();

    // Create initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates two new files in the same session
    let file_a_path = repo.path().join("file_a.txt");
    let file_b_path = repo.path().join("file_b.txt");

    fs::write(
        &file_a_path,
        "AI content for file A\nLine 2 from AI\nLine 3 from AI\n",
    )
    .unwrap();
    fs::write(
        &file_b_path,
        "AI content for file B\nLine 2 from AI\nLine 3 from AI\n",
    )
    .unwrap();

    // Single AI checkpoint covers both files (same AI session)
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    // First commit: only file_a.txt
    repo.git(&["add", "file_a.txt"]).unwrap();
    repo.commit("Add file A").unwrap();

    // Second commit: file_b.txt (this is where attribution is lost in issue #356)
    repo.git(&["add", "file_b.txt"]).unwrap();
    repo.commit("Add file B").unwrap();

    // Verify file_a.txt has correct AI attribution (this works)
    let mut file_a = repo.filename("file_a.txt");
    file_a.assert_lines_and_blame(crate::lines![
        "AI content for file A".ai(),
        "Line 2 from AI".ai(),
        "Line 3 from AI".ai(),
    ]);

    // Verify file_b.txt ALSO has correct AI attribution (this fails in issue #356)
    let mut file_b = repo.filename("file_b.txt");
    file_b.assert_lines_and_blame(crate::lines![
        "AI content for file B".ai(),
        "Line 2 from AI".ai(),
        "Line 3 from AI".ai(),
    ]);
}

/// Additional test for issue #356 with modifications instead of new files
#[test]
fn test_multi_file_batch_commits_modifications() {
    // Similar to above, but with modifications to existing files
    use std::fs;

    let repo = TestRepo::new();

    // Create initial files (human-authored)
    let file_a_path = repo.path().join("file_a.txt");
    let file_b_path = repo.path().join("file_b.txt");

    fs::write(&file_a_path, "Original content A\n").unwrap();
    fs::write(&file_b_path, "Original content B\n").unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial commit with both files")
        .unwrap();

    // AI modifies both files in the same session
    fs::write(&file_a_path, "Original content A\nAI added line A\n").unwrap();
    fs::write(&file_b_path, "Original content B\nAI added line B\n").unwrap();

    // Single AI checkpoint covers both modifications
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    // First commit: only file_a.txt
    repo.git(&["add", "file_a.txt"]).unwrap();
    repo.commit("Modify file A").unwrap();

    // Second commit: file_b.txt
    repo.git(&["add", "file_b.txt"]).unwrap();
    repo.commit("Modify file B").unwrap();

    // Verify both files have correct AI attribution
    let mut file_a = repo.filename("file_a.txt");
    file_a.assert_lines_and_blame(crate::lines![
        "Original content A".human(),
        "AI added line A".ai(),
    ]);

    let mut file_b = repo.filename("file_b.txt");
    file_b.assert_lines_and_blame(crate::lines![
        "Original content B".human(),
        "AI added line B".ai(), // This fails in issue #356 - shows as human
    ]);
}

#[test]
fn test_ai_edits_file_with_spaces_in_filename() {
    // Test that AI authorship tracking works correctly for files with spaces in the filename
    // This is a potential edge case that could fail if paths aren't properly quoted
    use std::fs;

    let repo = TestRepo::new();
    let file_path = repo.path().join("my test file.txt");

    // Initial commit: Create file with spaces in name
    fs::write(&file_path, "Line 1\nLine 2\nLine 3\n").unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial commit with spaced filename")
        .unwrap();

    // AI adds new lines to the file
    fs::write(&file_path, "Line 1\nLine 2\nAI Line 1\nAI Line 2\nLine 3\n").unwrap();

    // Mark the AI-authored content with mock_ai checkpoint
    repo.git_ai(&["checkpoint", "mock_ai", "my test file.txt"])
        .unwrap();

    repo.stage_all_and_commit("AI adds lines to file with spaces")
        .unwrap();

    // Verify line-by-line attribution
    let mut file = repo.filename("my test file.txt");
    file.assert_lines_and_blame(crate::lines![
        "Line 1".human(),
        "Line 2".human(),
        "AI Line 1".ai(),
        "AI Line 2".ai(),
        "Line 3".human(),
    ]);
}

/// Regression test: AI generates a full new file, then human deletes everything and
/// rewrites. The commit should report 100% human, not 100% AI.
///
/// The bug: when the human checkpoint has empty `line_attributions` but non-empty
/// byte-range `attributions` (all human), the fallback conversion in
/// `from_just_working_log` strips human lines (by design) producing an empty vec.
/// The empty result causes the code to `continue` without clearing the stale AI
/// attributions from the earlier checkpoint, so the commit is incorrectly tagged as AI.
#[test]
fn test_ai_generated_file_then_human_full_rewrite() {
    use sha2::{Digest, Sha256};

    let repo = TestRepo::new();
    let file_path = repo.path().join("jokes-cli.ts");

    // The final file content that will be committed (human-written).
    let human_content = "console.log('hello world');\nconsole.log('goodbye');";
    fs::write(&file_path, human_content).unwrap();
    repo.git(&["add", "-A"]).unwrap();

    // Compute blob SHAs for checkpoint entries
    let ai_content = "import * as readline from 'readline';\n\nconst jokes = [\n  \"Why don't scientists trust atoms?\",\n  \"An impasta!\"\n];";
    let ai_sha = format!(
        "{:x}",
        Sha256::new_with_prefix(ai_content.as_bytes()).finalize()
    );
    let human_sha = format!(
        "{:x}",
        Sha256::new_with_prefix(human_content.as_bytes()).finalize()
    );
    let human_len = human_content.len();

    // Directly write checkpoints.jsonl to replicate the exact real-world scenario:
    // 1) AI checkpoint with line_attributions covering the whole file
    // 2) Human checkpoint with empty line_attributions but non-empty byte-range attributions
    //
    // The author_id must match generate_short_hash(agent_id.id, agent_id.tool).
    // For tool="mock_ai", id="test_session": SHA256("mock_ai:test_session")[..16]
    let agent_author_id = "3bd30911a58cb074";
    // Determine the git dir and base commit for checkpoint storage.
    // In worktree mode .git is a gitlink file, so use rev-parse to resolve.
    // `--git-dir` may return a relative path; resolve it against the repo root
    // so that fs::create_dir_all works regardless of the process CWD.
    let git_dir_raw = repo
        .git(&["rev-parse", "--git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let git_dir_path = if std::path::Path::new(&git_dir_raw).is_absolute() {
        std::path::PathBuf::from(&git_dir_raw)
    } else {
        repo.path().join(&git_dir_raw)
    };
    let git_dir = git_dir_path.as_path();
    let base_commit = repo
        .git(&["rev-parse", "HEAD"])
        .unwrap_or_else(|_| "initial".to_string())
        .trim()
        .to_string();
    let checkpoints_dir = git_dir.join(format!("ai/working_logs/{}", base_commit));
    fs::create_dir_all(&checkpoints_dir).unwrap();
    let checkpoints_jsonl = format!(
        r#"{{"kind":"AiAgent","diff":"fake_diff_sha","author":"Test User","entries":[{{"file":"jokes-cli.ts","blob_sha":"{ai_sha}","attributions":[],"line_attributions":[{{"start_line":1,"end_line":6,"author_id":"{agent_author_id}","overrode":null}}]}}],"timestamp":1000,"transcript":{{"messages":[]}},"agent_id":{{"tool":"mock_ai","id":"test_session","model":"test"}},"agent_metadata":null,"line_stats":{{"additions":6,"deletions":0,"additions_sloc":5,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"development:1.1.23"}}
{{"kind":"Human","diff":"fake_diff_sha2","author":"Test User","entries":[{{"file":"jokes-cli.ts","blob_sha":"{human_sha}","attributions":[{{"start":0,"end":0,"author_id":"human","ts":2000}},{{"start":0,"end":{human_len},"author_id":"human","ts":2000}}],"line_attributions":[]}}],"timestamp":2000,"transcript":null,"agent_id":null,"agent_metadata":null,"line_stats":{{"additions":2,"deletions":6,"additions_sloc":2,"deletions_sloc":5}},"api_version":"checkpoint/1.0.0","git_ai_version":"development:1.1.23"}}"#
    );
    fs::write(
        checkpoints_dir.join("checkpoints.jsonl"),
        &checkpoints_jsonl,
    )
    .unwrap();

    // Commit
    repo.stage_all_and_commit("human rewrite").unwrap();

    // Assert everything is human-authored
    let mut file = repo.filename("jokes-cli.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('hello world');".human(),
        "console.log('goodbye');".human(),
    ]);
}

/// Regression test: known-human checkpoint must store the full git identity
/// ("Name <email>") in the HumanRecord, not just the name.
///
/// The test harness configures user.name = "Test User" and
/// user.email = "test@example.com", so the expected author field is
/// "Test User <test@example.com>".
#[test]
fn test_known_human_record_includes_email() {
    let repo = TestRepo::new();

    let file_path = repo.path().join("app.go");

    // AI writes the initial file
    repo.git_ai(&["checkpoint", "human", "app.go"]).unwrap();
    fs::write(&file_path, "func main() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "app.go"]).unwrap();
    repo.stage_all_and_commit("AI commit").unwrap();

    // Human edits the file
    fs::write(&file_path, "func main() {}\nfunc helper() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "app.go"])
        .unwrap();
    repo.stage_all_and_commit("Human commit").unwrap();

    let mut file = repo.filename("app.go");
    file.assert_committed_lines(crate::lines![
        "func main() {}".ai(),
        "func helper() {}".human(),
    ]);

    // Verify the HumanRecord has the full identity with email
    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&sha)
        .expect("human commit should have note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse note");
    assert!(
        !log.metadata.humans.is_empty(),
        "should have humans metadata"
    );
    for record in log.metadata.humans.values() {
        assert!(
            record.author.contains('<') && record.author.contains('>'),
            "HumanRecord.author should include email in angle brackets, got: {:?}",
            record.author
        );
        assert_eq!(
            record.author, "Test User <test@example.com>",
            "HumanRecord.author should be the full git identity"
        );
    }
}

#[test]
fn test_session_record_human_author_includes_email() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.rs");

    repo.git_ai(&["checkpoint", "human", "main.rs"]).unwrap();
    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.rs"]).unwrap();
    repo.stage_all_and_commit("AI commit").unwrap();

    let mut file = repo.filename("main.rs");
    file.assert_committed_lines(crate::lines!["fn main() {}".ai()]);

    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&sha)
        .expect("AI commit should have note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse note");
    assert!(
        !log.metadata.sessions.is_empty(),
        "should have sessions metadata"
    );
    for record in log.metadata.sessions.values() {
        let author = record
            .human_author
            .as_deref()
            .expect("human_author should be set");
        assert_eq!(
            author, "Test User <test@example.com>",
            "SessionRecord.human_author should be the full git identity"
        );
    }
}

/// Helper: assert every SessionRecord.human_author in the note for `sha` contains the email.
fn assert_session_authors_have_email(repo: &TestRepo, sha: &str) {
    let note = repo
        .read_authorship_note(sha)
        .unwrap_or_else(|| panic!("commit {} should have authorship note", &sha[..8]));
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse note");
    assert!(
        !log.metadata.sessions.is_empty(),
        "commit {} should have sessions metadata",
        &sha[..8]
    );
    for (id, record) in &log.metadata.sessions {
        let author = record
            .human_author
            .as_deref()
            .unwrap_or_else(|| panic!("session {} should have human_author", id));
        assert_eq!(
            author, "Test User <test@example.com>",
            "session {} human_author should be full git identity",
            id
        );
    }
}

/// Helper: assert every HumanRecord.author in the note for `sha` contains the email.
fn assert_human_records_have_email(repo: &TestRepo, sha: &str) {
    let note = repo
        .read_authorship_note(sha)
        .unwrap_or_else(|| panic!("commit {} should have authorship note", &sha[..8]));
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse note");
    assert!(
        !log.metadata.humans.is_empty(),
        "commit {} should have humans metadata",
        &sha[..8]
    );
    for (id, record) in &log.metadata.humans {
        assert_eq!(
            record.author, "Test User <test@example.com>",
            "human record {} author should be full git identity",
            id
        );
    }
}

/// Verify that SessionRecord.human_author includes email after checkout carryover.
/// Exercises daemon.rs working log carryover path (checkout_hooks → restore_working_log_carryover).
#[test]
fn test_checkout_carryover_preserves_author_email_in_session() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("work.txt");

    fs::write(repo.path().join("README.md"), "init\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    repo.git(&["branch", "feature"]).unwrap();

    // Create AI checkpoint on main (uncommitted)
    repo.git_ai(&["checkpoint", "human", "work.txt"]).unwrap();
    fs::write(&file_path, "AI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "work.txt"]).unwrap();

    // Checkout feature — working log carries over
    repo.git(&["checkout", "feature"]).unwrap();

    // Commit on feature branch
    repo.stage_all_and_commit("commit on feature").unwrap();

    let mut file = repo.filename("work.txt");
    file.assert_committed_lines(crate::lines!["AI line".ai()]);

    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_session_authors_have_email(&repo, &sha);
}

/// Verify that SessionRecord.human_author includes email after `git switch` carryover.
/// Exercises daemon.rs switch_hooks → restore_working_log_carryover path.
#[test]
fn test_switch_carryover_preserves_author_email_in_session() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("work.txt");

    fs::write(repo.path().join("README.md"), "init\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    repo.git(&["branch", "feature"]).unwrap();

    // Create AI checkpoint on main (uncommitted)
    repo.git_ai(&["checkpoint", "human", "work.txt"]).unwrap();
    fs::write(&file_path, "AI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "work.txt"]).unwrap();

    // Switch to feature — working log carries over
    repo.git(&["switch", "feature"]).unwrap();

    // Commit on feature branch
    repo.stage_all_and_commit("commit on feature").unwrap();

    let mut file = repo.filename("work.txt");
    file.assert_committed_lines(crate::lines!["AI line".ai()]);

    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_session_authors_have_email(&repo, &sha);
}

/// Verify that SessionRecord.human_author includes email after rebase rewrites the note.
/// Exercises daemon.rs apply_rewrite_prerequisites → post_commit path.
#[test]
fn test_rebase_rewrite_preserves_author_email_in_session() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("code.rs");

    // Base commit
    fs::write(&file_path, "fn base() {}\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // AI commit on top
    repo.git_ai(&["checkpoint", "human", "code.rs"]).unwrap();
    fs::write(&file_path, "fn base() {}\nfn ai() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "code.rs"]).unwrap();
    repo.stage_all_and_commit("ai commit").unwrap();

    let mut file = repo.filename("code.rs");
    file.assert_committed_lines(crate::lines![
        "fn base() {}".unattributed_human(),
        "fn ai() {}".ai(),
    ]);

    // Create a new base commit on a side branch to rebase onto
    repo.git(&["checkout", "-b", "new-base", "HEAD~1"]).unwrap();
    fs::write(repo.path().join("other.txt"), "other\n").unwrap();
    repo.stage_all_and_commit("new base commit").unwrap();
    let new_base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Go back to the AI commit's branch and rebase
    repo.git(&["checkout", "-"]).unwrap();
    repo.git(&["rebase", &new_base]).unwrap();

    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_session_authors_have_email(&repo, &sha);
}

/// Verify that HumanRecord.author includes email after rebase rewrites the note.
#[test]
fn test_rebase_rewrite_preserves_author_email_in_human_record() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("code.rs");

    // Base commit
    fs::write(&file_path, "fn base() {}\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // Known-human commit on top
    fs::write(&file_path, "fn base() {}\nfn human() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "code.rs"])
        .unwrap();
    repo.stage_all_and_commit("human commit").unwrap();

    let mut file = repo.filename("code.rs");
    file.assert_committed_lines(crate::lines![
        "fn base() {}".unattributed_human(),
        "fn human() {}".human(),
    ]);

    // Create a new base commit on a side branch
    repo.git(&["checkout", "-b", "new-base", "HEAD~1"]).unwrap();
    fs::write(repo.path().join("other.txt"), "other\n").unwrap();
    repo.stage_all_and_commit("new base commit").unwrap();
    let new_base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Go back and rebase
    repo.git(&["checkout", "-"]).unwrap();
    repo.git(&["rebase", &new_base]).unwrap();

    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_human_records_have_email(&repo, &sha);
}

/// Verify that `git-ai status` implicit checkpoint flows through to email in SessionRecord.
/// Exercises status.rs → checkpoint::run → post_commit path.
#[test]
fn test_status_checkpoint_preserves_author_email_in_session() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("app.py");

    // Base commit
    fs::write(&file_path, "print('hello')\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // AI edits
    repo.git_ai(&["checkpoint", "human", "app.py"]).unwrap();
    fs::write(&file_path, "print('hello')\nprint('ai')\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "app.py"]).unwrap();

    // Run git-ai status (triggers implicit human checkpoint internally)
    let _ = repo.git_ai(&["status", "--json"]);

    // Commit after status
    repo.stage_all_and_commit("post-status commit").unwrap();

    let mut file = repo.filename("app.py");
    file.assert_committed_lines(crate::lines![
        "print('hello')".unattributed_human(),
        "print('ai')".ai(),
    ]);

    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_session_authors_have_email(&repo, &sha);
}

crate::reuse_tests_in_worktree!(
    test_simple_additions_empty_repo,
    test_simple_additions_with_base_commit,
    test_simple_additions_on_top_of_ai_contributions,
    test_simple_additions_new_file_not_git_added,
    test_ai_human_interleaved_line_attribution,
    test_simple_ai_then_human_deletion,
    test_multiple_ai_checkpoints_with_human_deletions,
    test_complex_mixed_additions_and_deletions,
    test_partial_staging_filters_unstaged_lines,
    test_human_stages_some_ai_lines,
    test_ai_generated_file_then_human_full_rewrite,
    test_known_human_record_includes_email,
    test_session_record_human_author_includes_email,
    test_checkout_carryover_preserves_author_email_in_session,
    test_switch_carryover_preserves_author_email_in_session,
    test_rebase_rewrite_preserves_author_email_in_session,
    test_rebase_rewrite_preserves_author_email_in_human_record,
    test_status_checkpoint_preserves_author_email_in_session,
);
