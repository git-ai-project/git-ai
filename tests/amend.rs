#[macro_use]
mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

/// Test amending a commit by adding AI-authored lines at the top of the file.
#[test]
fn test_amend_add_lines_at_top() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // Initial file with human content
    file.set_contents(lines!["line 1", "line 2", "line 3", "line 4", "line 5"]);

    repo.git(&["add", "-A"]).unwrap();

    repo.commit("Initial commit").unwrap();

    // AI adds lines at the top
    file.insert_at(
        0,
        lines!["// AI added line 1".ai(), "// AI added line 2".ai()],
    );

    // Amend the commit WITHOUT staging the AI lines
    repo.git(&["commit", "--amend", "-m", "Initial commit (amended)"])
        .unwrap();

    // Now stage and commit the AI lines
    repo.stage_all_and_commit("Add AI lines").unwrap();

    // Verify AI authorship is preserved after the second commit
    file.assert_lines_and_blame(lines![
        "// AI added line 1".ai(),
        "// AI added line 2".ai(),
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "line 4".human(),
        "line 5".human()
    ]);
}

#[test]
fn test_amend_add_lines_in_middle() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // Initial file with human content
    file.set_contents(lines!["line 1", "line 2", "line 3", "line 4", "line 5"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds lines in the middle
    file.insert_at(
        2,
        lines!["// AI inserted line 1".ai(), "// AI inserted line 2".ai()],
    );

    // Amend the commit
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "-m", "Initial commit (amended)"])
        .unwrap();

    // Verify AI authorship is preserved
    file.assert_lines_and_blame(lines![
        "line 1".human(),
        "line 2".human(),
        "// AI inserted line 1".ai(),
        "// AI inserted line 2".ai(),
        "line 3".human(),
        "line 4".human(),
        "line 5".human()
    ]);
}

#[test]
fn test_amend_add_lines_at_bottom() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // Initial file with human content
    file.set_contents(lines!["line 1", "line 2", "line 3", "line 4", "line 5"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds lines at the bottom
    file.insert_at(
        5,
        lines!["// AI appended line 1".ai(), "// AI appended line 2".ai()],
    );

    // Amend the commit
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "-m", "Initial commit (amended)"])
        .unwrap();

    // Verify AI authorship is preserved
    file.assert_lines_and_blame(lines![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "line 4".human(),
        "line 5".human(),
        "// AI appended line 1".ai(),
        "// AI appended line 2".ai()
    ]);
}

#[test]
fn test_amend_multiple_changes() {
    let repo = TestRepo::new();
    let mut file = repo.filename("code.js");

    // Initial file with AI content
    file.set_contents(lines![
        "function example() {".ai(),
        "  return 42;".ai(),
        "}".ai()
    ]);
    repo.stage_all_and_commit("Add example function").unwrap();

    // AI adds header comment
    file.insert_at(0, lines!["// Header comment".ai()]);
    // After inserting at 0, the file now has 4 lines

    // AI adds documentation in middle (after line 2: "function example() {")
    file.insert_at(2, lines!["  // Added documentation".ai()]);
    // After inserting at 2, the file now has 5 lines

    // AI adds footer at bottom (at the end after "}")
    file.insert_at(5, lines!["// Footer".ai()]);

    // Amend the commit
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "-m", "Add example function (amended)"])
        .unwrap();

    // Verify all AI authorship is preserved
    file.assert_lines_and_blame(lines![
        "// Header comment".ai(),
        "function example() {".ai(),
        "  // Added documentation".ai(),
        "  return 42;".ai(),
        "}".ai(),
        "// Footer".ai()
    ]);
}

#[test]
fn test_amend_with_unstaged_ai_code_in_other_file() {
    let repo = TestRepo::new();

    // Create initial commit with fileA
    let mut file_a = repo.filename("fileA.txt");
    file_a.set_contents(lines!["fileA line 1", "fileA line 2"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create fileB with AI code but DON'T stage it yet
    let mut file_b = repo.filename("fileB.txt");
    file_b.set_contents_no_stage(lines![
        "// AI code in fileB".ai(),
        "function foo() {".ai(),
        "  return 'bar';".ai(),
        "}".ai()
    ]);

    // Modify fileA and amend the previous commit (fileB stays unstaged in working tree)
    file_a.insert_at(2, lines!["fileA line 3"]);
    repo.git(&["add", "fileA.txt"]).unwrap();
    repo.git(&["commit", "--amend", "-m", "Initial commit (amended)"])
        .unwrap();

    // Now stage and commit fileB in a new commit
    repo.stage_all_and_commit("Add fileB").unwrap();

    // Verify fileB has AI authorship
    file_b.assert_lines_and_blame(lines![
        "// AI code in fileB".ai(),
        "function foo() {".ai(),
        "  return 'bar';".ai(),
        "}".ai()
    ]);
}

/// Test that unstaged AI code in the tree is attributed after amending HEAD with a different file

#[test]
fn test_amend_preserves_unstaged_ai_attribution() {
    let repo = TestRepo::new();

    // Create initial commit with fileA
    let mut file_a = repo.filename("fileA.txt");
    file_a.set_contents(lines!["original content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Stage changes to fileA
    file_a.insert_at(1, lines!["staged addition"]);
    repo.git(&["add", "fileA.txt"]).unwrap();

    // Create fileB with unstaged AI code
    let mut file_b = repo.filename("fileB.txt");
    file_b.set_contents_no_stage(lines![
        "// Unstaged AI line 1".ai(),
        "// Unstaged AI line 2".ai(),
        "// Unstaged AI line 3".ai()
    ]);

    // Amend HEAD with fileA (fileB remains unstaged)
    repo.git(&["commit", "--amend", "-m", "Amended commit"])
        .unwrap();

    // Verify that fileB's AI attribution was saved in INITIAL attributions
    let initial = repo.current_working_logs().read_initial_attributions();
    assert!(
        initial.files.contains_key("fileB.txt"),
        "fileB.txt should be in initial attributions"
    );
    let file_b_attrs = &initial.files["fileB.txt"];
    assert_eq!(
        file_b_attrs.len(),
        1,
        "fileB should have 1 attribution range"
    );
    assert_eq!(file_b_attrs[0].start_line, 1);
    assert_eq!(file_b_attrs[0].end_line, 3);

    // Now stage and commit fileB
    repo.stage_all_and_commit("Add fileB").unwrap();

    // Verify fileB retains AI authorship
    file_b.assert_lines_and_blame(lines![
        "// Unstaged AI line 1".ai(),
        "// Unstaged AI line 2".ai(),
        "// Unstaged AI line 3".ai()
    ]);
}

/// Test amending with multiple files where some have unstaged AI changes

#[test]
fn test_amend_with_multiple_files_mixed_staging() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file1 = repo.filename("file1.txt");
    file1.set_contents(lines!["file1 original"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Stage changes to file1
    file1.insert_at(1, lines!["file1 staged"]);
    repo.git(&["add", "file1.txt"]).unwrap();

    // Create file2 with AI code (unstaged)
    let mut file2 = repo.filename("file2.txt");
    file2.set_contents_no_stage(lines!["// AI file2 line 1".ai(), "// AI file2 line 2".ai()]);

    // Create file3 with mixed AI and human code (unstaged)
    let mut file3 = repo.filename("file3.txt");
    file3.set_contents_no_stage(lines![
        "human line".human(),
        "// AI file3 line".ai(),
        "another human line".human()
    ]);

    // Amend with file1
    repo.git(&["commit", "--amend", "-m", "Amended with file1"])
        .unwrap();

    // Stage and commit file2 and file3
    repo.stage_all_and_commit("Add file2 and file3").unwrap();

    // Verify AI authorship is preserved
    file2.assert_lines_and_blame(lines!["// AI file2 line 1".ai(), "// AI file2 line 2".ai()]);

    file3.assert_lines_and_blame(lines![
        "human line".human(),
        "// AI file3 line".ai(),
        "another human line".human()
    ]);
}

/// Test amending with a partially staged AI file
/// Stage the first half, leave the second half unstaged
#[test]
fn test_amend_with_partially_staged_ai_file() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("code.txt");
    file.set_contents(lines!["// Initial line"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds 6 lines
    file.insert_at(
        1,
        lines![
            "// AI line 1".ai(),
            "// AI line 2".ai(),
            "// AI line 3".ai(),
            "// AI line 4".ai(),
            "// AI line 5".ai(),
            "// AI line 6".ai()
        ],
    );

    // Stage only the first 3 AI lines (using git add with patch would normally do this,
    // but we'll simulate by creating a version with only first 3 lines and staging that)
    let workdir = repo.path();
    let file_path = workdir.join("code.txt");

    // Write partial content (first 3 AI lines only + original)
    std::fs::write(
        &file_path,
        "// Initial line\n// AI line 1\n// AI line 2\n// AI line 3\n",
    )
    .unwrap();
    repo.git(&["add", "code.txt"]).unwrap();

    // Restore full content with all 6 AI lines
    std::fs::write(
        &file_path,
        "// Initial line\n// AI line 1\n// AI line 2\n// AI line 3\n// AI line 4\n// AI line 5\n// AI line 6\n"
    ).unwrap();

    // Amend the commit (only first 3 AI lines are staged)
    repo.git(&["commit", "--amend", "-m", "Initial commit (amended)"])
        .unwrap();

    // Now commit the remaining unstaged lines
    repo.stage_all_and_commit("Add remaining AI lines").unwrap();

    // Verify: first 3 AI lines should be attributed, and last 3 should also be attributed
    file.assert_lines_and_blame(lines![
        "// Initial line".human(),
        "// AI line 1".ai(),
        "// AI line 2".ai(),
        "// AI line 3".ai(),
        "// AI line 4".ai(),
        "// AI line 5".ai(),
        "// AI line 6".ai()
    ]);
}

/// Test amending with partially staged mixed AI/human file
#[test]
fn test_amend_with_partially_staged_mixed_content() {
    let repo = TestRepo::new();

    // Create initial file with human content
    let mut file = repo.filename("mixed.txt");
    file.set_contents(lines!["human line 1", "human line 2", "human end"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Stage only the first AI line and first human addition
    let workdir = repo.path();
    let file_path = workdir.join("mixed.txt");
    // add the line
    std::fs::write(
        &file_path,
        "human line 1\nhuman line 2\n// AI addition 1\nhuman end\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    repo.git(&["add", "mixed.txt"]).unwrap();

    std::fs::write(
        &file_path,
        "human line 1\nhuman line 2\n// AI addition 1\n// AI addition 2\nhuman end\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    repo.git(&["commit", "--amend", "-m", "Initial commit (amended)"])
        .unwrap();

    // Commit remaining unstaged content
    repo.stage_all_and_commit("Add remaining content").unwrap();

    // Verify all attributions preserved
    file.assert_lines_and_blame(lines![
        "human line 1".human(),
        "human line 2".human(),
        "// AI addition 1".ai(),
        "// AI addition 2".ai(),
        "human end".human(),
    ]);
}

/// Test amending where middle section of AI file is unstaged
#[test]
fn test_amend_with_unstaged_middle_section() {
    let repo = TestRepo::new();

    let mut file = repo.filename("function.txt");
    file.set_contents(lines!["// File header"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds multiple sections
    file.insert_at(
        1,
        lines![
            "// AI section 1 line 1".ai(),
            "// AI section 1 line 2".ai(),
            "// AI section 2 line 1".ai(),
            "// AI section 2 line 2".ai(),
            "// AI section 3 line 1".ai(),
            "// AI section 3 line 2".ai()
        ],
    );

    // Stage only sections 1 and 3 (leave section 2 unstaged)
    let workdir = repo.path();
    let file_path = workdir.join("function.txt");
    std::fs::write(
        &file_path,
        "// File header\n// AI section 1 line 1\n// AI section 1 line 2\n// AI section 3 line 1\n// AI section 3 line 2\n"
    ).unwrap();
    repo.git(&["add", "function.txt"]).unwrap();

    // Restore full content with middle section
    std::fs::write(
        &file_path,
        "// File header\n// AI section 1 line 1\n// AI section 1 line 2\n// AI section 2 line 1\n// AI section 2 line 2\n// AI section 3 line 1\n// AI section 3 line 2\n"
    ).unwrap();

    // Amend
    repo.git(&["commit", "--amend", "-m", "Initial commit (amended)"])
        .unwrap();

    // Commit remaining (middle section)
    repo.stage_all_and_commit("Add middle section").unwrap();

    // Verify all AI attributions preserved
    file.assert_lines_and_blame(lines![
        "// File header".human(),
        "// AI section 1 line 1".ai(),
        "// AI section 1 line 2".ai(),
        "// AI section 2 line 1".ai(),
        "// AI section 2 line 2".ai(),
        "// AI section 3 line 1".ai(),
        "// AI section 3 line 2".ai()
    ]);
}

#[test]
fn test_amend_repeated_round_trips_preserve_exact_line_authorship() {
    let repo = TestRepo::new();
    let mut file = repo.filename("code.js");

    file.set_contents(lines![
        "function example() {".ai(),
        "  return 42;".ai(),
        "}".ai()
    ]);
    repo.stage_all_and_commit("Add example function").unwrap();

    file.insert_at(0, lines!["// Header comment".ai()]);
    file.insert_at(2, lines!["  // Added documentation".ai()]);
    file.insert_at(5, lines!["// Footer".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&[
        "commit",
        "--amend",
        "-m",
        "Add example function (amended 1)",
    ])
    .unwrap();

    // Re-amend the same commit with mixed authorship changes.
    file.insert_at(0, lines!["// Human TODO".human()]);
    file.insert_at(7, lines!["// AI trailing note".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&[
        "commit",
        "--amend",
        "-m",
        "Add example function (amended 2)",
    ])
    .unwrap();

    file.assert_lines_and_blame(lines![
        "// Human TODO".human(),
        "// Header comment".ai(),
        "function example() {".ai(),
        "  // Added documentation".ai(),
        "  return 42;".ai(),
        "}".ai(),
        "// Footer".ai(),
        "// AI trailing note".ai()
    ]);
}

worktree_test_wrappers! {
    test_amend_add_lines_at_top,
    test_amend_add_lines_in_middle,
    test_amend_add_lines_at_bottom,
    test_amend_multiple_changes,
    test_amend_with_unstaged_ai_code_in_other_file,
    test_amend_preserves_unstaged_ai_attribution,
    test_amend_with_multiple_files_mixed_staging,
    test_amend_with_partially_staged_ai_file,
    test_amend_with_partially_staged_mixed_content,
    test_amend_with_unstaged_middle_section,
}
