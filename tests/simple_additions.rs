#[macro_use]
mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use std::fs;

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

    file.set_contents(lines!["base_line", ""]);

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
        // ai_modified3 is ai, but it's not considered committed, because adding the subsequent uncommitted lines also added a newline char to this line
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
        // ai_line8 is ai, but it's not considered committed, because adding the subsequent uncommitted lines also added a newline char to this line
    ]);
}

#[test]
fn test_multiple_ai_sessions_with_partial_staging() {
    // Multiple AI sessions, but only one has staged changes
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(lines!["line1", "line2", "line3"]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // First AI session adds lines and they get staged
    file.insert_at(
        3,
        lines!["ai1_line1".ai(), "ai1_line2".ai(), "ai1_line3".ai()],
    );

    file.stage();

    // Second AI session adds lines but they DON'T get staged
    file.insert_at(
        6,
        lines!["ai2_line1".ai(), "ai2_line2".ai(), "ai2_line3".ai()],
    );

    let commit = repo.commit("Commit first AI session only").unwrap();
    assert_eq!(commit.authorship_log.attestations.len(), 1);

    // Only check committed lines (second AI session unstaged)
    file.assert_committed_lines(lines![
        "line1".human(),
        "line2".human(),
        "line3".human(),
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

    file.set_contents(lines!["line1", "line2", "line3", "line4", ""]);

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
        // line5 is human, but it's not considered committed, because adding line 6+ also added a newline char to line 5
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
        // line 5 is ai, but it's not considered committed, because adding line 6+ also added a newline char to line 5
    ]);
}

#[test]
fn test_unstaged_ai_lines_saved_to_working_log() {
    // Test that unstaged AI-authored lines are saved to the working log for the next commit
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(lines!["line1", "line2", "line3", ""]);

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
fn test_with_duplicate_lines() {
    // This test verifies that squash merge correctly preserves AI authorship for duplicate lines
    let repo = TestRepo::new();
    let mut file = repo.filename("helpers.rs");

    // Create master branch with first function (human-authored)
    file.set_contents(lines![
        "pub fn format_string(s: &str) -> String {",
        "    s.to_uppercase()",
        "}",
    ]);
    repo.stage_all_and_commit("Add format_string function")
        .unwrap();

    file = repo.filename("helpers.rs");
    file.assert_lines_and_blame(lines![
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
    file.assert_lines_and_blame(lines![
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
    file.assert_lines_and_blame(lines![
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
    file.assert_lines_and_blame(lines![
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
    file.assert_lines_and_blame(lines![
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
    readme.set_contents(lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates two new files in the same session
    let file_a_path = repo.path().join("file_a.txt");
    let file_b_path = repo.path().join("file_b.txt");

    fs::write(&file_a_path, "AI content for file A\nLine 2 from AI\nLine 3 from AI\n").unwrap();
    fs::write(&file_b_path, "AI content for file B\nLine 2 from AI\nLine 3 from AI\n").unwrap();

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
    file_a.assert_lines_and_blame(lines![
        "AI content for file A".ai(),
        "Line 2 from AI".ai(),
        "Line 3 from AI".ai(),
    ]);

    // Verify file_b.txt ALSO has correct AI attribution (this fails in issue #356)
    let mut file_b = repo.filename("file_b.txt");
    file_b.assert_lines_and_blame(lines![
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
    repo.stage_all_and_commit("Initial commit with both files").unwrap();

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
    file_a.assert_lines_and_blame(lines![
        "Original content A".human(),
        "AI added line A".ai(),
    ]);

    let mut file_b = repo.filename("file_b.txt");
    file_b.assert_lines_and_blame(lines![
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
    fs::write(
        &file_path,
        "Line 1\nLine 2\nAI Line 1\nAI Line 2\nLine 3\n",
    )
    .unwrap();

    // Mark the AI-authored content with mock_ai checkpoint
    repo.git_ai(&["checkpoint", "mock_ai", "my test file.txt"])
        .unwrap();

    repo.stage_all_and_commit("AI adds lines to file with spaces")
        .unwrap();

    // Verify line-by-line attribution
    let mut file = repo.filename("my test file.txt");
    file.assert_lines_and_blame(lines![
        "Line 1".human(),
        "Line 2".human(),
        "AI Line 1".ai(),
        "AI Line 2".ai(),
        "Line 3".human(),
    ]);
}
