use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// Test conflict resolution behavior: verifies rebase completes successfully.
///
/// **Note on conflict resolution attribution**:
/// When a rebase conflict occurs and a human manually resolves it, perfect attribution
/// is impossible without explicit checkpoints during resolution. V3 attempts to:
/// 1. Use working log data from parent commit (may be stale/irrelevant)
/// 2. Fall back to transforming original commit's note through diff hunks
///
/// Both approaches have limitations:
/// - Working log from parent may not reflect the actual resolution
/// - Hunk transformation assumes line-number correspondence, not content matching
///
/// These tests verify that conflict resolution **completes successfully** and **produces
/// some reasonable attribution**, but do not prescribe exact attribution behavior, which
/// depends on complex heuristics and may have edge case bugs.
///
/// For production use: AI coding agents should checkpoint after resolving conflicts to
/// provide explicit attribution data.
#[test]
fn test_conflict_resolution_completes_successfully() {
    let repo = TestRepo::new();

    let mut file = repo.filename("config.js");
    file.set_contents(crate::lines!["// Config file"]);
    repo.stage_all_and_commit("Base commit").unwrap();

    let default_branch = repo.current_branch();

    // Feature branch: AI adds code
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.set_contents(crate::lines![
        "// Config file",
        "const timeout = 5000;".ai(),
        "const retries = 3;".ai()
    ]);
    repo.stage_all_and_commit("AI adds config").unwrap();

    // Verify AI attribution before rebase
    file.assert_committed_lines(crate::lines![
        "// Config file".unattributed_human(),
        "const timeout = 5000;".ai(),
        "const retries = 3;".ai()
    ]);

    // Main branch: conflicting change
    repo.git(&["checkout", &default_branch]).unwrap();
    file.set_contents(crate::lines![
        "// Config file",
        "const timeout = 1000;",
        "const maxConnections = 10;"
    ]);
    repo.stage_all_and_commit("Main changes config").unwrap();

    // Rebase - should conflict
    repo.git(&["checkout", "feature"]).unwrap();
    let rebase_result = repo.git(&["rebase", &default_branch]);
    assert!(rebase_result.is_err(), "Rebase should conflict");

    // Human resolves conflict
    let resolved = "\
// Config file
const timeout = 5000;
const maxConnections = 10;
const retries = 3;
";
    fs::write(repo.path().join("config.js"), resolved).unwrap();
    repo.git(&["add", "config.js"]).unwrap();

    // Verify rebase completes successfully
    let continue_result =
        repo.git_with_env(&["rebase", "--continue"], &[("GIT_EDITOR", "true")], None);
    assert!(
        continue_result.is_ok(),
        "Rebase should complete after conflict resolution"
    );

    // Verify file has some attribution (exact attribution not prescribed due to complexity)
    // The important thing is that the commit has a valid authorship note and doesn't crash
    let resolved_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo.read_authorship_note(&resolved_sha);
    assert!(
        note.is_some(),
        "Resolved commit should have an authorship note"
    );
}

/// Test non-conflicting rebase preserves attribution correctly.
///
/// This test establishes the baseline: when there's NO conflict, attribution is preserved
/// perfectly through v3's hunk transformation.
#[test]
fn test_non_conflicting_rebase_preserves_attribution() {
    let repo = TestRepo::new();

    let mut file = repo.filename("utils.js");
    file.set_contents(crate::lines!["// Utils"]);
    repo.stage_all_and_commit("Base").unwrap();

    let default_branch = repo.current_branch();

    // Feature: AI adds function
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.set_contents(crate::lines![
        "// Utils",
        "function clamp(x, min, max) {".ai(),
        "    return Math.max(min, Math.min(max, x));".ai(),
        "}".ai()
    ]);
    repo.stage_all_and_commit("AI adds clamp").unwrap();

    file.assert_committed_lines(crate::lines![
        "// Utils".unattributed_human(),
        "function clamp(x, min, max) {".ai(),
        "    return Math.max(min, Math.min(max, x));".ai(),
        "}".ai()
    ]);

    // Main: non-conflicting change (different file)
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut other = repo.filename("other.js");
    other.set_contents(crate::lines!["// Other file"]);
    repo.stage_all_and_commit("Main adds other file").unwrap();

    // Rebase - no conflict
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Attribution preserved exactly
    file.assert_committed_lines(crate::lines![
        "// Utils".unattributed_human(),
        "function clamp(x, min, max) {".ai(),
        "    return Math.max(min, Math.min(max, x));".ai(),
        "}".ai()
    ]);
}

/// Test skip during conflict - verifies authorship tracking survives rebase --skip.
#[test]
fn test_conflict_skip_preserves_remaining_commits() {
    let repo = TestRepo::new();

    let mut file1 = repo.filename("file1.js");
    let mut file2 = repo.filename("file2.js");
    file1.set_contents(crate::lines!["// File 1"]);
    repo.stage_all_and_commit("Base").unwrap();

    let default_branch = repo.current_branch();

    // Feature: two commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file1.set_contents(crate::lines!["// File 1", "const x = 1;".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();

    file2.set_contents(crate::lines!["const y = 2;".ai()]);
    repo.stage_all_and_commit("AI commit 2").unwrap();

    file2.assert_committed_lines(crate::lines!["const y = 2;".ai()]);

    // Main: conflicting change to file1
    repo.git(&["checkout", &default_branch]).unwrap();
    file1.set_contents(crate::lines!["// File 1", "const x = 999;"]);
    repo.stage_all_and_commit("Main changes file1").unwrap();

    // Rebase - first commit conflicts
    repo.git(&["checkout", "feature"]).unwrap();
    let rebase_result = repo.git(&["rebase", &default_branch]);
    assert!(rebase_result.is_err(), "Should conflict on first commit");

    // Skip the conflicting commit
    repo.git(&["rebase", "--skip"]).unwrap();

    // Second commit should be rebased with attribution preserved
    file2.assert_committed_lines(crate::lines!["const y = 2;".ai()]);
}

/// Test abort during conflict - verifies attribution restored to pre-rebase state.
#[test]
fn test_conflict_abort_restores_attribution() {
    let repo = TestRepo::new();

    let mut file = repo.filename("code.js");
    file.set_contents(crate::lines!["// Code"]);
    repo.stage_all_and_commit("Base").unwrap();

    let default_branch = repo.current_branch();

    // Feature: AI adds code
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.set_contents(crate::lines!["// Code", "function test() {}".ai()]);
    repo.stage_all_and_commit("AI commit").unwrap();

    let feature_sha_before = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note_before = repo.read_authorship_note(&feature_sha_before);

    file.assert_committed_lines(crate::lines![
        "// Code".unattributed_human(),
        "function test() {}".ai()
    ]);

    // Main: conflicting change
    repo.git(&["checkout", &default_branch]).unwrap();
    file.set_contents(crate::lines!["// Code", "function test() { return 1; }"]);
    repo.stage_all_and_commit("Main changes").unwrap();

    // Rebase - conflict
    repo.git(&["checkout", "feature"]).unwrap();
    let rebase_result = repo.git(&["rebase", &default_branch]);
    assert!(rebase_result.is_err(), "Should conflict");

    // Abort
    repo.git(&["rebase", "--abort"]).unwrap();

    // Verify attribution restored to original state
    let feature_sha_after = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_eq!(
        feature_sha_before, feature_sha_after,
        "Should be back to original commit"
    );

    let note_after = repo.read_authorship_note(&feature_sha_after);
    assert_eq!(
        note_before, note_after,
        "Attribution should be unchanged after abort"
    );

    file.assert_committed_lines(crate::lines![
        "// Code".unattributed_human(),
        "function test() {}".ai()
    ]);
}
