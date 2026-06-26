/// Regression tests for issue #1444: attribution should not be fully lost when a human
/// edits a file after an AI checkpoint fires but before the commit — without a
/// known_human checkpoint being triggered.
use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// AI writes lines 1-3, human appends lines 4-6 to same file without firing any
/// checkpoint, then commits everything. AI-written lines should remain attributed to AI.
/// Adjacent uncheckpointed lines get AI attribution via edge recovery (up to
/// EDGE_EXTENSION_MAX_LINES = 3 lines).
#[test]
fn test_ai_writes_then_human_appends_no_checkpoint() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("example.txt");

    // Human sets up a base file (untracked — no checkpoint fired)
    let base = "\
Base line
";
    fs::write(&file_path, base).unwrap();
    repo.stage_all_and_commit("Base commit").unwrap();

    // AI pre-edit snapshot (legacy human = untracked, as the AI preset takes before editing)
    repo.git_ai(&["checkpoint", "human", "example.txt"])
        .unwrap();

    // AI writes lines 2-4
    let after_ai = "\
Base line
AI line 1
AI line 2
AI line 3
";
    fs::write(&file_path, after_ai).unwrap();
    // AI post-edit checkpoint
    repo.git_ai(&["checkpoint", "mock_ai", "example.txt"])
        .unwrap();

    // Human appends lines 5-7 WITHOUT firing any checkpoint
    let after_human = "\
Base line
AI line 1
AI line 2
AI line 3
Human line 1
Human line 2
Human line 3
";
    fs::write(&file_path, after_human).unwrap();

    // Commit everything — no known_human checkpoint was fired for the human lines.
    // Edge recovery attributes up to 3 adjacent uncheckpointed lines to the neighboring
    // AI session, so all three human-appended lines are attributed to AI here.
    repo.stage_all_and_commit("Mixed commit").unwrap();

    let mut file = repo.filename("example.txt");
    file.assert_committed_lines(crate::lines![
        "Base line".unattributed_human(),
        "AI line 1".ai(),
        "AI line 2".ai(),
        "AI line 3".ai(),
        "Human line 1".ai(),
        "Human line 2".ai(),
        "Human line 3".ai(),
    ]);
}

/// AI writes lines 1-5, human modifies line 3 content without firing a checkpoint,
/// then commits. Unmodified AI lines should retain AI attribution; modified line is
/// untracked.
#[test]
fn test_ai_writes_then_human_modifies_within_ai_range() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("example.txt");

    let base = "\
Base line
";
    fs::write(&file_path, base).unwrap();
    repo.stage_all_and_commit("Base commit").unwrap();

    repo.git_ai(&["checkpoint", "human", "example.txt"])
        .unwrap();

    let after_ai = "\
Base line
AI line 1
AI line 2
AI line 3
AI line 4
AI line 5
";
    fs::write(&file_path, after_ai).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "example.txt"])
        .unwrap();

    // Human modifies line 3 in-place (no checkpoint)
    let after_human = "\
Base line
AI line 1
AI line 2
Human modified line 3
AI line 4
AI line 5
";
    fs::write(&file_path, after_human).unwrap();

    repo.stage_all_and_commit("Mixed commit").unwrap();

    let mut file = repo.filename("example.txt");
    file.assert_committed_lines(crate::lines![
        "Base line".unattributed_human(),
        "AI line 1".ai(),
        "AI line 2".ai(),
        "Human modified line 3".unattributed_human(),
        "AI line 4".ai(),
        "AI line 5".ai(),
    ]);
}

/// AI writes lines, human inserts 2 lines above the AI region (shifting AI lines down)
/// without firing a checkpoint, then commits. AI lines (now at new positions) should
/// retain AI attribution. Adjacent uncheckpointed inserted lines get AI attribution via
/// edge recovery (up to EDGE_EXTENSION_MAX_LINES = 3 lines from the run end toward AI).
#[test]
fn test_ai_writes_then_human_inserts_above() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("example.txt");

    let base = "\
Base line
";
    fs::write(&file_path, base).unwrap();
    repo.stage_all_and_commit("Base commit").unwrap();

    repo.git_ai(&["checkpoint", "human", "example.txt"])
        .unwrap();

    let after_ai = "\
Base line
AI line 1
AI line 2
AI line 3
";
    fs::write(&file_path, after_ai).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "example.txt"])
        .unwrap();

    // Human inserts 2 lines between base and AI lines (no checkpoint)
    let after_human = "\
Base line
Human insert 1
Human insert 2
AI line 1
AI line 2
AI line 3
";
    fs::write(&file_path, after_human).unwrap();

    // Commit everything. Edge recovery takes the 2 inserted lines (≤3) adjacent to the
    // AI block and attributes them to the same AI session.
    repo.stage_all_and_commit("Mixed commit").unwrap();

    let mut file = repo.filename("example.txt");
    file.assert_committed_lines(crate::lines![
        "Base line".unattributed_human(),
        "Human insert 1".ai(),
        "Human insert 2".ai(),
        "AI line 1".ai(),
        "AI line 2".ai(),
        "AI line 3".ai(),
    ]);
}

/// Regression guard: when a human has REAL uncommitted changes (staged only some AI
/// lines), the unstaged changes should still be correctly deferred to INITIAL for the
/// next commit rather than being incorrectly attributed to this commit.
#[test]
fn test_real_uncommitted_changes_still_go_to_initial() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("example.txt");

    let base = "\
Base line
";
    fs::write(&file_path, base).unwrap();
    repo.stage_all_and_commit("Base commit").unwrap();

    // AI writes 4 lines
    repo.git_ai(&["checkpoint", "human", "example.txt"])
        .unwrap();
    let after_ai = "\
Base line
AI line 1
AI line 2
AI line 3
AI line 4
";
    fs::write(&file_path, after_ai).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "example.txt"])
        .unwrap();

    // Stage only first 3 AI lines (4th remains unstaged)
    let staged = "\
Base line
AI line 1
AI line 2
AI line 3
";
    fs::write(&file_path, staged).unwrap();
    repo.git(&["add", "example.txt"]).unwrap();

    // Restore the full content (4th line is "dirty" / unstaged after git add)
    fs::write(&file_path, after_ai).unwrap();

    repo.commit("Partial AI commit").unwrap();

    // First commit: only lines 1-3 are committed, line 4 is uncommitted (INITIAL)
    let mut file = repo.filename("example.txt");
    file.assert_committed_lines(crate::lines![
        "Base line".unattributed_human(),
        "AI line 1".ai(),
        "AI line 2".ai(),
        "AI line 3".ai(),
    ]);

    // Now commit the remaining unstaged line
    repo.stage_all_and_commit("Remaining AI line").unwrap();

    file.assert_committed_lines(crate::lines![
        "Base line".unattributed_human(),
        "AI line 1".ai(),
        "AI line 2".ai(),
        "AI line 3".ai(),
        "AI line 4".ai(),
    ]);
}
