/// Regression test for https://github.com/git-ai-project/git-ai/issues/1138
///
/// When AI inserts new content between two existing blocks (e.g. a new method
/// between two existing methods in a Java file), all inserted lines should be
/// attributed as AI. The bug reported that one line ends up as
/// `unknown_additions` instead of `ai_additions`.
use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// Reproduces the exact scenario from issue #1138:
/// 1. Create a Java file with two methods (each with comments).
/// 2. AI inserts a new method with a comment between the two existing methods.
/// 3. All inserted lines must be AI-attributed; `unknown_additions` should be 0.
#[test]
fn test_ai_insert_between_existing_methods_issue_1138() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("Calculator.java");

    // Step 1 — human writes a file with two methods and comments.
    let initial = "\
public class Calculator {

    // Method to add two numbers
    public int add(int a, int b) {
        return a + b;
    }

    // Method to subtract two numbers
    public int subtract(int a, int b) {
        return a - b;
    }
}
";
    fs::write(&file_path, initial).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "Calculator.java"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit with two methods")
        .unwrap();

    let mut file = repo.filename("Calculator.java");
    file.assert_committed_lines(crate::lines![
        "public class Calculator {".human(),
        "".human(),
        "    // Method to add two numbers".human(),
        "    public int add(int a, int b) {".human(),
        "        return a + b;".human(),
        "    }".human(),
        "".human(),
        "    // Method to subtract two numbers".human(),
        "    public int subtract(int a, int b) {".human(),
        "        return a - b;".human(),
        "    }".human(),
        "}".human(),
    ]);

    // Step 2 — AI inserts a new method (with comment) between add() and subtract().
    // First, take a pre-edit "human" checkpoint to capture the state before AI edits.
    repo.git_ai(&["checkpoint", "human", "Calculator.java"])
        .unwrap();

    let after_ai_insert = "\
public class Calculator {

    // Method to add two numbers
    public int add(int a, int b) {
        return a + b;
    }

    // Method to multiply two numbers
    public int multiply(int a, int b) {
        return a * b;
    }

    // Method to subtract two numbers
    public int subtract(int a, int b) {
        return a - b;
    }
}
";
    fs::write(&file_path, after_ai_insert).unwrap();
    // Post-edit AI checkpoint.
    repo.git_ai(&["checkpoint", "mock_ai", "Calculator.java"])
        .unwrap();

    repo.stage_all_and_commit("AI adds multiply method")
        .unwrap();

    // Step 3 — assert every line's attribution.
    file.assert_committed_lines(crate::lines![
        "public class Calculator {".human(),
        "".human(),
        "    // Method to add two numbers".human(),
        "    public int add(int a, int b) {".human(),
        "        return a + b;".human(),
        "    }".human(),
        "".human(), // original blank line (human) OR new blank line (ai) — see note below
        "    // Method to multiply two numbers".ai(), // AI-inserted
        "    public int multiply(int a, int b) {".ai(), // AI-inserted
        "        return a * b;".ai(), // AI-inserted
        "    }".ai(), // AI-inserted
        "".ai(),    // AI-inserted blank line
        "    // Method to subtract two numbers".human(),
        "    public int subtract(int a, int b) {".human(),
        "        return a - b;".human(),
        "    }".human(),
        "}".human(),
    ]);

    // Also verify stats: all added lines should be ai_additions, none unknown.
    let stats = repo.stats().expect("stats should succeed");
    assert_eq!(
        stats.unknown_additions, 0,
        "Issue #1138: AI-inserted lines between existing methods should not be unknown. Stats: {:?}",
        stats
    );
    assert!(
        stats.ai_additions >= 5,
        "Expected at least 5 ai_additions for the inserted method block, got {}. Stats: {:?}",
        stats.ai_additions,
        stats
    );
}

/// Variant: AI inserts a single line (a comment) between two existing lines.
/// Even a single inserted line must be fully AI-attributed.
#[test]
fn test_ai_insert_single_line_between_existing_lines_issue_1138() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("notes.txt");

    let initial = "\
First existing line
Second existing line
Third existing line
";
    fs::write(&file_path, initial).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "notes.txt"])
        .unwrap();
    repo.stage_all_and_commit("Initial three lines").unwrap();

    let mut file = repo.filename("notes.txt");
    file.assert_committed_lines(crate::lines![
        "First existing line".human(),
        "Second existing line".human(),
        "Third existing line".human(),
    ]);

    // Pre-edit checkpoint
    repo.git_ai(&["checkpoint", "human", "notes.txt"]).unwrap();

    let after_ai = "\
First existing line
Second existing line
AI inserted line
Third existing line
";
    fs::write(&file_path, after_ai).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "notes.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI inserts one line").unwrap();

    file.assert_committed_lines(crate::lines![
        "First existing line".human(),
        "Second existing line".human(),
        "AI inserted line".ai(),
        "Third existing line".human(),
    ]);

    let stats = repo.stats().expect("stats should succeed");
    assert_eq!(
        stats.unknown_additions, 0,
        "Single AI-inserted line between existing lines should not be unknown. Stats: {:?}",
        stats
    );
    assert_eq!(
        stats.ai_additions, 1,
        "Expected exactly 1 ai_addition. Stats: {:?}",
        stats
    );
}

/// Variant: AI inserts multiple blocks at different positions in the same edit.
/// All inserted content must be AI-attributed.
///
/// This test REPRODUCES issue #1138: when AI inserts content at multiple
/// positions in a single edit, the line ranges for later insertion blocks
/// are truncated, causing the tail lines to be misattributed as unknown
/// (Test User) instead of AI. Specifically, the closing brace and trailing
/// blank line of the second inserted method block are wrongly unattributed.
///
/// Remove `#[ignore]` once the underlying line-range bug is fixed.
#[test]
#[ignore]
fn test_ai_insert_multiple_blocks_between_existing_issue_1138() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("Service.java");

    let initial = "\
public class Service {

    public void methodA() {
        // do A
    }

    public void methodB() {
        // do B
    }

    public void methodC() {
        // do C
    }
}
";
    fs::write(&file_path, initial).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "Service.java"])
        .unwrap();
    repo.stage_all_and_commit("Initial three methods").unwrap();

    let mut file = repo.filename("Service.java");
    file.assert_committed_lines(crate::lines![
        "public class Service {".human(),
        "".human(),
        "    public void methodA() {".human(),
        "        // do A".human(),
        "    }".human(),
        "".human(),
        "    public void methodB() {".human(),
        "        // do B".human(),
        "    }".human(),
        "".human(),
        "    public void methodC() {".human(),
        "        // do C".human(),
        "    }".human(),
        "}".human(),
    ]);

    // Pre-edit checkpoint
    repo.git_ai(&["checkpoint", "human", "Service.java"])
        .unwrap();

    // AI inserts a helper between A and B, and another helper between B and C.
    let after_ai = "\
public class Service {

    public void methodA() {
        // do A
    }

    // AI helper between A and B
    public void helperAB() {
        // inserted by AI
    }

    public void methodB() {
        // do B
    }

    // AI helper between B and C
    public void helperBC() {
        // inserted by AI
    }

    public void methodC() {
        // do C
    }
}
";
    fs::write(&file_path, after_ai).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "Service.java"])
        .unwrap();
    repo.stage_all_and_commit("AI adds two helper methods")
        .unwrap();

    file.assert_committed_lines(crate::lines![
        "public class Service {".human(),
        "".human(),
        "    public void methodA() {".human(),
        "        // do A".human(),
        "    }".human(),
        "".human(),
        "    // AI helper between A and B".ai(),
        "    public void helperAB() {".ai(),
        "        // inserted by AI".ai(),
        "    }".ai(),
        "".ai(),
        "    public void methodB() {".human(),
        "        // do B".human(),
        "    }".human(),
        "".human(),
        "    // AI helper between B and C".ai(),
        "    public void helperBC() {".ai(),
        "        // inserted by AI".ai(),
        "    }".ai(),
        "".ai(),
        "    public void methodC() {".human(),
        "        // do C".human(),
        "    }".human(),
        "}".human(),
    ]);

    let stats = repo.stats().expect("stats should succeed");
    assert_eq!(
        stats.unknown_additions, 0,
        "AI-inserted blocks between existing methods should not be unknown. Stats: {:?}",
        stats
    );
    assert!(
        stats.ai_additions >= 10,
        "Expected at least 10 ai_additions for two inserted method blocks, got {}. Stats: {:?}",
        stats.ai_additions,
        stats
    );
}
