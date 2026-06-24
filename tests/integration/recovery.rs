//! Integration tests for post-commit attribution recovery:
//! - Solver 1: bash mtime/ctime correlation
//! - Solver 2: AI edge extension
use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// A file produced by a (simulated) bash command, whose mtime falls within the
/// recorded bash-checkpoint window, is recovered as AI-attributed.
#[test]
fn test_bash_recovery_attributes_untracked_file() {
    let repo = TestRepo::new_with_bash_recovery();

    let path = repo.path().join("script_out.txt");
    fs::write(&path, "line one\nline two\n").unwrap();
    // Record a bash checkpoint whose window brackets the file mtime (now).
    repo.seed_bash_checkpoint("echo hi > script_out.txt");

    repo.git(&["add", "."]).unwrap();
    repo.commit("add generated file").unwrap();

    let mut file = repo.filename("script_out.txt");
    file.assert_committed_lines(crate::lines!["line one".ai(), "line two".ai()]);
}

/// A file whose mtime is far from any recorded bash checkpoint stays untracked.
#[test]
fn test_bash_recovery_timing_miss_stays_untracked() {
    let repo = TestRepo::new_with_bash_recovery();

    let path = repo.path().join("orphan.txt");
    fs::write(&path, "no owner\n").unwrap();
    // Bash checkpoint window is an hour in the past → no correlation.
    repo.seed_bash_checkpoint_at("echo hi", -3600);

    repo.git(&["add", "."]).unwrap();
    repo.commit("add orphan file").unwrap();

    let mut file = repo.filename("orphan.txt");
    file.assert_committed_lines(crate::lines!["no owner".unattributed_human()]);
}

/// An untracked line directly below an AI-attributed block is absorbed into
/// that AI session by the edge-extension solver.
#[test]
fn test_edge_extension_absorbs_trailing_untracked_line() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("m.txt");

    // AI writes lines 1-2.
    fs::write(&file_path, "ai one\nai two\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "m.txt"]).unwrap();

    // A third line lands with no checkpoint (untracked), directly below the AI
    // block.
    fs::write(&file_path, "ai one\nai two\nuntracked edge\n").unwrap();

    repo.stage_all_and_commit("commit with trailing edge")
        .unwrap();

    let mut file = repo.filename("m.txt");
    file.assert_committed_lines(crate::lines![
        "ai one".ai(),
        "ai two".ai(),
        "untracked edge".ai(), // extended into the AI session
    ]);
}

/// An untracked line adjacent only to human (known-human) code is NOT extended.
#[test]
fn test_edge_extension_does_not_steal_human_adjacent() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("h.txt");

    fs::write(&file_path, "human one\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "h.txt"])
        .unwrap();

    fs::write(&file_path, "human one\nuntracked\n").unwrap();
    repo.stage_all_and_commit("commit with human + untracked")
        .unwrap();

    let mut file = repo.filename("h.txt");
    file.assert_committed_lines(crate::lines![
        "human one".human(),
        "untracked".unattributed_human(), // NOT extended (adjacent to human)
    ]);
}
