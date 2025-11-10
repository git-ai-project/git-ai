#[macro_use]
mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

#[test]
fn test_simple_additions_empty_repo() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["Line1", "Line 2".ai(), "Line 3".ai(),]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    file.assert_lines_and_blame(lines!["Line1".human(), "Line 2".ai(), "Line 3".ai(),]);
}

#[test]
fn test_simple_additions_with_base_commit() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["Base line 1", "Base line 2"]);

    repo.stage_all_and_commit("Base commit").unwrap();

    file.insert_at(
        2,
        lines!["NEW LINEs From Claude!".ai(), "Hello".ai(), "World".ai(),],
    );

    repo.stage_all_and_commit("AI additions").unwrap();

    file.assert_lines_and_blame(lines![
        "Base line 1".human(),
        "Base line 2".human(),
        "NEW LINEs From Claude!".ai(),
        "Hello".ai(),
        "World".ai(),
    ]);
}

#[test]
fn test_simple_additions_on_top_of_ai_contributions() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["Line 1", "Line 2", "Line 3"]);

    repo.stage_all_and_commit("Base commit").unwrap();

    file.insert_at(3, lines!["AI Line 1".ai(), "AI Line 2".ai(),]);

    repo.stage_all_and_commit("AI commit").unwrap();

    file.replace_at(3, "HUMAN EDITED AI LINE".human());

    repo.stage_all_and_commit("Human edits AI").unwrap();

    file.assert_lines_and_blame(lines![
        "Line 1".human(),
        "Line 2".human(),
        "Line 3".human(),
        "HUMAN EDITED AI LINE".human(),
        "AI Line 2".ai(),
    ]);
}

#[test]
fn test_simple_additions_new_file_not_git_added() {
    let repo = TestRepo::new();
    let mut file = repo.filename("new_file.txt");

    // Create a new file with human lines, then add AI lines before any git add
    file.set_contents(lines![
        "Line 1 from human",
        "Line 2 from human",
        "Line 3 from human",
        "Line 4 from AI".ai(),
        "Line 5 from AI".ai(),
    ]);

    let commit = repo.stage_all_and_commit("Initial commit").unwrap();

    // All lines should be attributed correctly
    assert!(commit.authorship_log.attestations.len() > 0);

    file.assert_lines_and_blame(lines![
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

    file.set_contents(lines!["Base line"]);

    repo.stage_all_and_commit("Base commit").unwrap();

    file.insert_at(
        1,
        lines!["AI Line 1".ai(), "Human Line 1".human(), "AI Line 2".ai()],
    );

    repo.stage_all_and_commit("Interleaved commit").unwrap();

    file.assert_lines_and_blame(lines![
        "Base line".human(),
        "AI Line 1".ai(),
        "Human Line 1".human(),
        "AI Line 2".ai(),
    ]);
}

#[test]
fn test_simple_ai_then_human_deletion() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["Line 1", "Line 2", "Line 3", "Line 4", "Line 5"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    file.insert_at(5, lines!["AI Line".ai()]);

    repo.stage_all_and_commit("AI adds line").unwrap();

    file.delete_at(5);

    let commit = repo.stage_all_and_commit("Human deletes AI line").unwrap();

    // The authorship log should have no attestations since we only deleted lines
    assert_eq!(commit.authorship_log.attestations.len(), 0);

    file.assert_lines_and_blame(lines![
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

    file.set_contents(lines!["Base"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    file.insert_at(1, lines!["AI1 Line 1".ai(), "AI1 Line 2".ai()]);
    file.insert_at(3, lines!["AI2 Line 1".ai(), "AI2 Line 2".ai()]);

    // Delete the first AI session's lines (indices 1 and 2)
    file.delete_range(1, 3);

    let commit = repo.stage_all_and_commit("Complex commit").unwrap();

    // Should only have AI2's lines attributed (now at indices 1 and 2 after deletion)
    assert_eq!(commit.authorship_log.attestations.len(), 1);

    file.assert_lines_and_blame(lines!["Base".human(), "AI2 Line 1".ai(), "AI2 Line 2".ai(),]);
}

#[test]
fn test_complex_mixed_additions_and_deletions() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines![
        "Line 1", "Line 2", "Line 3", "Line 4", "Line 5", "Line 6", "Line 7", "Line 8", "Line 9",
        "Line 10",
    ]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI deletes lines 2-3 and replaces with new content (delete at index 1, 2 items)
    file.delete_range(1, 3);
    file.insert_at(
        1,
        lines!["NEW LINE A".ai(), "NEW LINE B".ai(), "NEW LINE C".ai(),],
    );

    // AI inserts at the end
    file.insert_at(11, lines!["END LINE 1".ai(), "END LINE 2".ai(),]);

    let commit = repo.stage_all_and_commit("Complex edits").unwrap();

    // Should have lines 2-4 and the last 2 lines attributed to AI
    assert_eq!(commit.authorship_log.attestations.len(), 1);

    file.assert_lines_and_blame(lines![
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
        "Line 10".human(),
        "END LINE 1".ai(),
        "END LINE 2".ai(),
    ]);
}

#[test]
fn test_ai_adds_lines_multiple_commits() {
    // Test AI adding lines across multiple commits
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(lines!["base_line"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    file.insert_at(
        1,
        lines!["ai_line1".ai(), "ai_line2".ai(), "ai_line3".ai(),],
    );

    repo.stage_all_and_commit("AI adds first batch").unwrap();

    file.insert_at(4, lines!["ai_line4".ai(), "ai_line5".ai(),]);

    repo.stage_all_and_commit("AI adds second batch").unwrap();

    file.assert_lines_and_blame(lines![
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

    file.set_contents(lines!["line1", "line2", "line3"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI modifies lines 2-3 and we stage immediately
    file.replace_at(1, "ai_modified2".ai());
    file.replace_at(2, "ai_modified3".ai());

    file.stage();

    // Now AI adds more lines that won't be staged
    file.insert_at(3, lines!["unstaged_line1".ai(), "unstaged_line2".ai()]);

    let commit = repo.commit("Partial staging").unwrap();

    // The commit should only include the modifications, not the unstaged additions
    assert_eq!(commit.authorship_log.attestations.len(), 1);

    // Only check committed lines (unstaged lines will be ignored)
    file.assert_committed_lines(lines![
        "line1".human(),
        "ai_modified2".ai(),
        "ai_modified3".ai(),
    ]);
}

#[test]
fn test_human_stages_some_ai_lines() {
    // Test where AI adds multiple lines but human only stages some of them
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(lines!["line1", "line2", "line3"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds lines 4-8
    file.insert_at(
        3,
        lines![
            "ai_line4".ai(),
            "ai_line5".ai(),
            "ai_line6".ai(),
            "ai_line7".ai(),
            "ai_line8".ai(),
        ],
    );

    file.stage();

    // Human adds an unstaged line
    file.insert_at(8, lines!["human_unstaged".human()]);

    let commit = repo.commit("Partial AI commit").unwrap();
    assert_eq!(commit.authorship_log.attestations.len(), 1);

    // Only check committed lines (unstaged human line will be ignored)
    file.assert_committed_lines(lines![
        "line1".human(),
        "line2".human(),
        "line3".human(),
        "ai_line4".ai(),
        "ai_line5".ai(),
        "ai_line6".ai(),
        "ai_line7".ai(),
        "ai_line8".ai(),
    ]);
}

#[test]
fn test_multiple_ai_sessions_with_partial_staging() {
    // Multiple AI sessions, but only one has staged changes
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(lines!["line1", "line2"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // First AI session adds lines and they get staged
    file.insert_at(2, lines!["ai1_line1".ai(), "ai1_line2".ai()]);

    file.stage();

    // Second AI session adds lines but they DON'T get staged
    file.insert_at(4, lines!["ai2_line1".ai(), "ai2_line2".ai()]);

    let commit = repo.commit("Commit first AI session only").unwrap();
    assert_eq!(commit.authorship_log.attestations.len(), 1);

    // Only check committed lines (second AI session unstaged)
    file.assert_committed_lines(lines![
        "line1".human(),
        "line2".human(),
        "ai1_line1".ai(),
        "ai1_line2".ai(),
    ]);
}

#[test]
fn test_ai_adds_then_commits_in_batches() {
    // AI adds lines in multiple batches, committing separately
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(lines!["line1", "line2", "line3", "line4"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds first batch of lines
    file.insert_at(4, lines!["ai_line5".ai(), "ai_line6".ai(), "ai_line7".ai()]);
    file.stage();

    repo.commit("Add lines 5-7").unwrap();

    // AI adds second batch of lines
    file.insert_at(
        7,
        lines!["ai_line8".ai(), "ai_line9".ai(), "ai_line10".ai()],
    );

    repo.stage_all_and_commit("Add lines 8-10").unwrap();

    file.assert_lines_and_blame(lines![
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

    file.set_contents(lines!["line1", "line2", "line3", "line4", "line5"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI modifies some lines
    file.replace_at(1, "ai_modified_line2".ai());
    file.replace_at(3, "ai_modified_line4".ai());

    // Stage only the modifications
    file.stage();

    // AI adds more lines that won't be staged
    file.insert_at(5, lines!["ai_line6".ai(), "ai_line7".ai(), "ai_line8".ai()]);

    let commit = repo.commit("Partial staging").unwrap();

    // Only the staged modifications should be in the commit
    assert_eq!(commit.authorship_log.attestations.len(), 1);

    // Only check committed lines
    file.assert_committed_lines(lines![
        "line1".human(),
        "ai_modified_line2".ai(),
        "line3".human(),
        "ai_modified_line4".ai(),
        "line5".human(),
    ]);
}

#[test]
fn test_unstaged_changes_not_committed() {
    // Test that unstaged changes don't appear in the commit
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(lines!["line1", "line2", "line3"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds lines at the end and stages them
    file.insert_at(3, lines!["ai_line4".ai(), "ai_line5".ai()]);
    file.stage();

    // AI adds more lines that won't be staged
    file.insert_at(5, lines!["unstaged_line6".ai(), "unstaged_line7".ai()]);

    let commit = repo.commit("Commit only staged lines").unwrap();

    // Only the staged lines should be in the commit
    assert!(commit.authorship_log.attestations.len() > 0);

    // Only check committed lines
    file.assert_committed_lines(lines![
        "line1".human(),
        "line2".human(),
        "line3".human(),
        "ai_line4".ai(),
        "ai_line5".ai(),
    ]);
}

#[test]
fn test_unstaged_ai_lines_saved_to_working_log() {
    // Test that unstaged AI-authored lines are saved to the working log for the next commit
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(lines!["line1", "line2", "line3"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds lines 4-7 and stages some
    file.insert_at(3, lines!["ai_line4".ai(), "ai_line5".ai()]);
    file.stage();

    // AI adds more lines that won't be staged
    file.insert_at(5, lines!["ai_line6".ai(), "ai_line7".ai()]);

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
    file.assert_lines_and_blame(lines![
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
    readme.set_contents(lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a brand new file with planets
    let mut file = repo.filename("planets.txt");
    file.set_contents(lines![
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

    file.assert_lines_and_blame(lines![
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
fn test_mock_ai_with_pathspecs() {
    let repo = TestRepo::new();
    let mut file1 = repo.filename("file1.txt");
    let mut file2 = repo.filename("file2.txt");

    // Create initial state
    file1.set_contents(lines!["File1 Line 1", "File1 Line 2"]);
    file2.set_contents(lines!["File2 Line 1", "File2 Line 2"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make changes to both files
    file1.insert_at(2, lines!["File1 AI Line".ai()]);
    file2.insert_at(2, lines!["File2 Human Line"]);

    // Use mock_ai with pathspec to only checkpoint file1.txt
    repo.git_ai(&["checkpoint", "mock_ai", "file1.txt"])
        .unwrap();

    // Commit the changes
    repo.stage_all_and_commit("Second commit").unwrap();

    // file1 should have AI attribution for the new line
    file1.assert_lines_and_blame(lines![
        "File1 Line 1".human(),
        "File1 Line 2".human(),
        "File1 AI Line".ai(),
    ]);

    // file2 should be all human since we didn't checkpoint it with mock_ai
    file2.assert_lines_and_blame(lines![
        "File2 Line 1".human(),
        "File2 Line 2".human(),
        "File2 Human Line".human(),
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

    // Human checkpoint
    repo.git_ai(&["checkpoint"]).unwrap();

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
    file.assert_lines_and_blame(lines![
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
