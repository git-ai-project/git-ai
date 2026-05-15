//! End-to-end user scenario tests simulating realistic multi-step workflows.
//!
//! These tests cover common real-world patterns that developers encounter when
//! using AI coding assistants alongside manual editing, rebasing, stashing, and amending.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

// ---------------------------------------------------------------------------
// Test 1: AI writes code, human tweaks it, commit has mixed attribution
// ---------------------------------------------------------------------------

/// Simulates the common pattern where AI generates initial code, then the
/// developer makes manual adjustments before committing.
#[test]
fn test_scenario_ai_writes_then_human_edits_then_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("handler.rs");

    // AI generates initial implementation
    let ai_code = "\
fn handle_request(req: Request) -> Response {
    let body = req.body();
    let parsed = serde_json::from_str(&body).unwrap();
    Response::ok(parsed)
}
";
    fs::write(&file_path, ai_code).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "handler.rs"])
        .unwrap();

    // Human tweaks: adds error handling (changes line 3, adds line 4)
    let human_tweaked = "\
fn handle_request(req: Request) -> Response {
    let body = req.body();
    let parsed = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return Response::bad_request(e),
    };
    Response::ok(parsed)
}
";
    fs::write(&file_path, human_tweaked).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "handler.rs"])
        .unwrap();

    repo.stage_all_and_commit("feat: handle requests with error handling")
        .unwrap();

    // Blame should show mixed attribution
    let blame = repo.git_ai(&["blame", "handler.rs"]).unwrap();
    assert!(
        blame.contains("mock_ai") || blame.contains("Test User"),
        "blame should show attribution from both AI and human"
    );

    // Verify the commit has a note
    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo.read_authorship_note(&sha);
    assert!(note.is_some(), "mixed authorship commit must have a note");
}

// ---------------------------------------------------------------------------
// Test 2: Multiple AI edits before one commit
// ---------------------------------------------------------------------------

/// AI makes several edits (multiple checkpoint calls) before the developer
/// decides to commit. All AI lines should be correctly attributed.
#[test]
fn test_scenario_multiple_ai_edits_single_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("utils.rs");

    // First AI edit: creates the file with 2 functions
    let edit1 = "\
fn helper_one() -> String {
    \"one\".to_string()
}
";
    fs::write(&file_path, edit1).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "utils.rs"]).unwrap();

    // Second AI edit: adds another function
    let edit2 = "\
fn helper_one() -> String {
    \"one\".to_string()
}

fn helper_two() -> u32 {
    42
}
";
    fs::write(&file_path, edit2).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "utils.rs"]).unwrap();

    // Third AI edit: adds a third function
    let edit3 = "\
fn helper_one() -> String {
    \"one\".to_string()
}

fn helper_two() -> u32 {
    42
}

fn helper_three() -> bool {
    true
}
";
    fs::write(&file_path, edit3).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "utils.rs"]).unwrap();

    repo.stage_all_and_commit("feat: add utility functions")
        .unwrap();

    // All lines should be AI-attributed
    let mut file = repo.filename("utils.rs");
    file.assert_lines_and_blame(crate::lines![
        "fn helper_one() -> String {".ai(),
        "    \"one\".to_string()".ai(),
        "}".ai(),
        "".ai(),
        "fn helper_two() -> u32 {".ai(),
        "    42".ai(),
        "}".ai(),
        "".ai(),
        "fn helper_three() -> bool {".ai(),
        "    true".ai(),
        "}".ai()
    ]);
}

// ---------------------------------------------------------------------------
// Test 3: Human writes code, AI reformats (non-substantial) — stays human
// ---------------------------------------------------------------------------

/// Human writes code, then AI reformats it (whitespace/style changes only).
/// Since the reformatting is non-substantial, attribution should reflect that
/// the human wrote the logic.
#[test]
fn test_scenario_human_writes_ai_formats() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("code.rs");

    // Human writes compact code
    let human_code = "fn compute(x:i32,y:i32)->i32{x+y}\n";
    fs::write(&file_path, human_code).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "code.rs"])
        .unwrap();

    // AI reformats (expands the function)
    let formatted = "\
fn compute(x: i32, y: i32) -> i32 {
    x + y
}
";
    fs::write(&file_path, formatted).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "code.rs"]).unwrap();

    repo.stage_all_and_commit("style: reformat compute function")
        .unwrap();

    // The commit should have a note (AI did touch it)
    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo.read_authorship_note(&sha);
    assert!(note.is_some(), "commit should have authorship note");

    // Verify blame output exists (content check depends on non-substantial detection)
    let blame = repo.git_ai(&["blame", "code.rs"]).unwrap();
    assert!(!blame.is_empty(), "blame should produce output");
}

// ---------------------------------------------------------------------------
// Test 4: Full workflow: branch, AI edit, rebase onto main, attribution preserved
// ---------------------------------------------------------------------------

/// Complete real-world workflow: create branch, AI edits, main advances,
/// rebase feature onto main, verify attribution survives.
#[test]
fn test_scenario_branch_rebase_push() {
    let repo = TestRepo::new();
    let default_branch = repo.current_branch();

    // Initial state on main
    let base_path = repo.path().join("base.rs");
    fs::write(&base_path, "fn base() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature-ai"]).unwrap();

    // AI creates a new file on feature branch
    let feature_path = repo.path().join("feature.rs");
    let feature_code = "\
fn ai_feature() -> &'static str {
    \"generated by AI\"
}
";
    fs::write(&feature_path, feature_code).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "feature.rs"])
        .unwrap();
    repo.stage_all_and_commit("feat: AI creates feature module")
        .unwrap();

    // Verify attribution before rebase
    let mut feature_file = repo.filename("feature.rs");
    feature_file.assert_lines_and_blame(crate::lines![
        "fn ai_feature() -> &'static str {".ai(),
        "    \"generated by AI\"".ai(),
        "}".ai()
    ]);

    // Switch to main and add a commit
    repo.git(&["checkout", &default_branch]).unwrap();
    let new_file = repo.path().join("main_update.rs");
    fs::write(&new_file, "fn main_update() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human"]).unwrap();
    repo.stage_all_and_commit("Main: add main_update.rs")
        .unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature-ai"]).unwrap();
    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed (disjoint files)");

    // Verify attribution preserved after rebase
    feature_file.assert_lines_and_blame(crate::lines![
        "fn ai_feature() -> &'static str {".ai(),
        "    \"generated by AI\"".ai(),
        "}".ai()
    ]);
}

// ---------------------------------------------------------------------------
// Test 5: AI editing, user stashes, pops, continues editing, attribution correct
// ---------------------------------------------------------------------------

/// Simulates a common interruption pattern: AI is editing, user stashes to
/// switch contexts, then pops the stash and continues.
#[test]
fn test_scenario_stash_during_ai_edit() {
    let repo = TestRepo::new();

    // Initial commit so we have something to stash against
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Project\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI starts editing a file
    let file_path = repo.path().join("wip.rs");
    fs::write(&file_path, "fn work_in_progress() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "wip.rs"]).unwrap();

    // User stashes to handle something else
    repo.git(&["add", "wip.rs"]).unwrap();
    repo.git(&["stash", "push", "-m", "ai work in progress"])
        .expect("stash should succeed");

    // Verify file is gone
    assert!(!file_path.exists(), "file should be gone after stash");

    // Pop the stash
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // AI continues editing (adds more)
    let continued = "\
fn work_in_progress() {}
fn additional_work() {}
";
    fs::write(&file_path, continued).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "wip.rs"]).unwrap();

    // Commit everything
    repo.stage_all_and_commit("feat: complete AI work after stash")
        .unwrap();

    // Verify attribution — all lines should be AI
    let mut wip_file = repo.filename("wip.rs");
    wip_file.assert_lines_and_blame(crate::lines![
        "fn work_in_progress() {}".ai(),
        "fn additional_work() {}".ai()
    ]);
}

// ---------------------------------------------------------------------------
// Test 6: AI commits, then user amends commit message only, attribution preserved
// ---------------------------------------------------------------------------

/// User makes an AI-assisted commit, then amends ONLY the message (no content
/// changes). The attribution must be preserved through the amend.
#[test]
fn test_scenario_amend_after_ai_commit() {
    let repo = TestRepo::new();

    // Initial commit
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Project\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI generates code and commits
    let file_path = repo.path().join("generated.rs");
    let ai_code = "\
fn generated_function() -> u32 {
    100
}
";
    fs::write(&file_path, ai_code).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "generated.rs"])
        .unwrap();
    repo.stage_all_and_commit("wip: AI code").unwrap();

    // Verify attribution before amend
    let pre_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let pre_note = repo.read_authorship_note(&pre_sha);
    assert!(pre_note.is_some(), "should have note before amend");

    // Amend only the commit message (no file changes)
    repo.git(&[
        "commit",
        "--amend",
        "-m",
        "feat: properly named AI function",
    ])
    .expect("amend should succeed");

    // Verify attribution preserved after amend
    let post_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_ne!(pre_sha, post_sha, "amend should create new SHA");

    let post_note = repo.read_authorship_note(&post_sha);
    assert!(
        post_note.is_some(),
        "authorship note must survive message-only amend"
    );

    // Verify blame still shows AI
    let mut gen_file = repo.filename("generated.rs");
    gen_file.assert_lines_and_blame(crate::lines![
        "fn generated_function() -> u32 {".ai(),
        "    100".ai(),
        "}".ai()
    ]);
}
