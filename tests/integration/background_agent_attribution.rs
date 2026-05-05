use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// When git-ai runs inside a no-hooks background agent (simulated via
/// `GIT_AI_CLOUD_AGENT=1` on the daemon), commits should be attributed wholly
/// to the detected AI tool even though no checkpoints were fired.
#[test]
fn test_no_hooks_agent_all_lines_attributed() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_CLOUD_AGENT", "1")]);

    fs::write(repo.path().join("file.txt"), "alpha\nbeta\ngamma\n").unwrap();
    repo.stage_all_and_commit("first commit").unwrap();

    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(crate::lines!["alpha".ai(), "beta".ai(), "gamma".ai()]);
}

/// Multiple files in a single commit are all attributed.
#[test]
fn test_no_hooks_agent_multiple_files() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_CLOUD_AGENT", "1")]);

    fs::write(repo.path().join("a.txt"), "line a\n").unwrap();
    fs::write(repo.path().join("b.txt"), "line b\n").unwrap();
    repo.stage_all_and_commit("multi file").unwrap();

    let mut a = repo.filename("a.txt");
    a.assert_committed_lines(crate::lines!["line a".ai()]);
    let mut b = repo.filename("b.txt");
    b.assert_committed_lines(crate::lines!["line b".ai()]);
}

/// When lines already have explicit attribution (e.g. from a KnownHuman
/// checkpoint fired by an IDE extension), only the unattributed "holes"
/// get the background agent attribution.
#[test]
fn test_no_hooks_agent_preserves_existing_attribution() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_CLOUD_AGENT", "1")]);

    // Seed commit so we have a base
    fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    repo.stage_all_and_commit("seed").unwrap();

    // Simulate: human types some lines (KnownHuman checkpoint), then agent adds more
    // without firing its own checkpoint.
    let content = "human typed this\nagent added this\nagent also added this\n";
    fs::write(repo.path().join("mixed.txt"), content).unwrap();
    // Fire a KnownHuman checkpoint for the file BEFORE the agent edits
    // (simulates IDE extension detecting human keystrokes for just the first line)
    fs::write(repo.path().join("mixed.txt"), "human typed this\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "mixed.txt"])
        .unwrap();

    // Now the agent adds more lines without firing a checkpoint
    fs::write(
        repo.path().join("mixed.txt"),
        "human typed this\nagent added this\nagent also added this\n",
    )
    .unwrap();
    repo.stage_all_and_commit("mixed edit").unwrap();

    let mut file = repo.filename("mixed.txt");
    file.assert_committed_lines(crate::lines![
        "human typed this".human(),
        "agent added this".ai(),
        "agent also added this".ai(),
    ]);
}

/// When the background agent modifies an existing file (appends lines),
/// only the new lines get attributed.
#[test]
fn test_no_hooks_agent_append_to_existing_file() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_CLOUD_AGENT", "1")]);

    fs::write(repo.path().join("file.txt"), "original\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(crate::lines!["original".ai()]);

    fs::write(repo.path().join("file.txt"), "original\nnew line\n").unwrap();
    repo.stage_all_and_commit("append").unwrap();

    file.assert_committed_lines(crate::lines!["original".ai(), "new line".ai()]);
}

/// Negative control: same shape, no env var. Lines that arrived without any
/// checkpoint are untracked.
#[test]
fn test_without_background_agent_env_lines_are_untracked() {
    let repo = TestRepo::new();

    fs::write(repo.path().join("plain.txt"), "alpha\nbeta\n").unwrap();
    repo.stage_all_and_commit("no agent").unwrap();

    let mut file = repo.filename("plain.txt");
    file.assert_committed_lines(crate::lines![
        "alpha".unattributed_human(),
        "beta".unattributed_human(),
    ]);
}

/// With-hooks agents (Claude Code remote, Cursor) should NOT trigger
/// blanket attribution — they fire their own checkpoints.
#[test]
fn test_with_hooks_agent_does_not_blanket_attribute() {
    let repo = TestRepo::new_with_daemon_env(&[("CLAUDE_CODE_REMOTE", "true")]);

    fs::write(repo.path().join("file.txt"), "line\n").unwrap();
    repo.stage_all_and_commit("commit").unwrap();

    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(crate::lines!["line".unattributed_human()]);
}

/// Rebase must NOT re-attribute lines to the background agent. The original
/// attribution (from the commit being rebased) should be preserved through
/// the rebase operation.
#[test]
fn test_rebase_does_not_reattribute_to_bg_agent() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_CLOUD_AGENT", "1")]);

    // Commit 1: base
    fs::write(repo.path().join("file.txt"), "base line\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // Commit 2: AI adds a line (attributed to bg agent via our new logic)
    fs::write(repo.path().join("file.txt"), "base line\nai line\n").unwrap();
    repo.stage_all_and_commit("ai commit").unwrap();

    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(crate::lines!["base line".ai(), "ai line".ai()]);

    // Create a side branch from base and add a different file
    repo.git(&["checkout", "-b", "side", "HEAD~1"]).unwrap();
    fs::write(repo.path().join("other.txt"), "side content\n").unwrap();
    repo.stage_all_and_commit("side commit").unwrap();

    // Go back to main and rebase onto side
    repo.git(&["checkout", "-"]).unwrap();
    repo.git(&["rebase", "side"]).unwrap();

    // After rebase, the AI line should still be attributed to AI (the same
    // agent), not re-attributed or doubled.
    file.assert_committed_lines(crate::lines!["base line".ai(), "ai line".ai()]);
}

/// Amend preserves existing attribution and fills holes for new lines.
#[test]
fn test_amend_preserves_existing_and_attributes_new_lines() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_CLOUD_AGENT", "1")]);

    // Commit with explicit AI checkpoint
    fs::write(repo.path().join("file.txt"), "original\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(crate::lines!["original".ai()]);

    // Amend: add a line without firing a checkpoint
    fs::write(repo.path().join("file.txt"), "original\namended line\n").unwrap();
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "--amend", "--no-edit"]).unwrap();

    // Original AI attribution preserved; new line attributed to bg agent
    file.assert_committed_lines(crate::lines!["original".ai(), "amended line".ai()]);
}
