use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// When git-ai runs inside a no-hooks background agent (simulated via
/// `GIT_AI_CLOUD_AGENT=1` on the daemon), commits should be attributed wholly
/// to the detected AI tool even though no checkpoints were fired.
#[test]
fn test_no_hooks_background_agent_commit_attributed_to_ai() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_CLOUD_AGENT", "1")]);

    fs::write(repo.path().join("seed.txt"), "seed line\n").unwrap();
    repo.stage_all_and_commit("seed").unwrap();
    let mut seed_file = repo.filename("seed.txt");
    seed_file.assert_committed_lines(crate::lines!["seed line".ai()]);

    fs::write(repo.path().join("cloud.txt"), "alpha\nbeta\ngamma\n").unwrap();
    repo.stage_all_and_commit("cloud agent edit").unwrap();

    let mut file = repo.filename("cloud.txt");
    file.assert_committed_lines(crate::lines!["alpha".ai(), "beta".ai(), "gamma".ai()]);
}

/// Negative control: same shape, no env var. Lines that arrived without any
/// checkpoint are untracked.
#[test]
fn test_without_background_agent_env_lines_are_untracked() {
    let repo = TestRepo::new();

    fs::write(repo.path().join("seed.txt"), "seed line\n").unwrap();
    repo.stage_all_and_commit("seed").unwrap();
    let mut seed_file = repo.filename("seed.txt");
    seed_file.assert_committed_lines(crate::lines!["seed line".unattributed_human()]);

    fs::write(repo.path().join("plain.txt"), "alpha\nbeta\n").unwrap();
    repo.stage_all_and_commit("no agent").unwrap();

    let mut file = repo.filename("plain.txt");
    file.assert_committed_lines(crate::lines![
        "alpha".unattributed_human(),
        "beta".unattributed_human(),
    ]);
}

/// When the background agent modifies an existing file, only the new/changed
/// lines get attributed — pre-existing lines remain unaffected.
#[test]
fn test_background_agent_partial_file_edit() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_CLOUD_AGENT", "1")]);

    fs::write(repo.path().join("file.txt"), "original\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    fs::write(repo.path().join("file.txt"), "original\nnew line\n").unwrap();
    repo.stage_all_and_commit("add line").unwrap();

    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(crate::lines!["original".ai(), "new line".ai()]);
}
