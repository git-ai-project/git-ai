use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{DaemonTestScope, GitTestMode, TestRepo};
use std::fs;

/// When git-ai's wrapper runs inside a no-hooks background agent (here
/// simulated via `GIT_AI_CLOUD_AGENT=1`), commits should be attributed wholly
/// to the detected AI tool even though no AI checkpoints were ever fired.
///
/// The wrapper subprocess inherits the env var the test sets via
/// `stage_all_and_commit_with_env` and fires a synthetic AI pre-commit
/// checkpoint before proxying to git. The test therefore runs in
/// WrapperDaemon mode — pure Daemon mode bypasses the wrapper entirely and
/// can't see per-invocation env vars.
#[test]
fn test_no_hooks_background_agent_commit_attributed_to_ai() {
    let repo = TestRepo::new_with_mode_and_daemon_scope(
        GitTestMode::WrapperDaemon,
        DaemonTestScope::Dedicated,
    );

    fs::write(repo.path().join("seed.txt"), "seed line\n").unwrap();
    repo.stage_all_and_commit("seed").unwrap();

    fs::write(repo.path().join("cloud.txt"), "alpha\nbeta\ngamma\n").unwrap();
    repo.stage_all_and_commit_with_env("cloud agent edit", &[("GIT_AI_CLOUD_AGENT", "1")])
        .unwrap();

    let mut file = repo.filename("cloud.txt");
    file.assert_committed_lines(crate::lines!["alpha".ai(), "beta".ai(), "gamma".ai(),]);
}

/// Negative control: same shape, no env var. Lines that arrived without any
/// checkpoint are flagged as untracked (legacy human) — confirms the previous
/// test isn't passing because of an unrelated default.
#[test]
fn test_without_background_agent_env_lines_are_untracked() {
    let repo = TestRepo::new_with_mode_and_daemon_scope(
        GitTestMode::WrapperDaemon,
        DaemonTestScope::Dedicated,
    );

    fs::write(repo.path().join("seed.txt"), "seed line\n").unwrap();
    repo.stage_all_and_commit("seed").unwrap();

    fs::write(repo.path().join("plain.txt"), "alpha\nbeta\n").unwrap();
    repo.stage_all_and_commit("no agent").unwrap();

    let mut file = repo.filename("plain.txt");
    file.assert_committed_lines(crate::lines![
        "alpha".unattributed_human(),
        "beta".unattributed_human(),
    ]);
}
