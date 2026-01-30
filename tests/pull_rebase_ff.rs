#[macro_use]
mod repos;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

/// Helper struct that provides a local repo with an upstream containing seeded commits.
/// The local repo is initially behind the upstream.
struct PullTestSetup {
    /// The local clone - initially behind upstream after setup
    local: TestRepo,
    /// The bare upstream repository (kept alive for the duration of the test)
    #[allow(dead_code)]
    upstream: TestRepo,
    /// SHA of the second commit (upstream is ahead by this)
    upstream_sha: String,
}

worktree_test_wrappers! {
    test_fast_forward_pull_preserves_ai_attribution,
    test_pull_rebase_autostash_preserves_ai_attribution,
    test_pull_rebase_autostash_with_mixed_attribution,
    test_pull_rebase_autostash_via_git_config,
    test_fast_forward_pull_without_local_changes,
}

/// Creates a test setup for pull scenarios:
/// 1. Creates upstream (bare) and local (clone) repos
/// 2. Makes an initial commit in local, pushes to upstream  
/// 3. Makes a second commit in local, pushes to upstream
/// 4. Resets local back to initial commit (so local is behind upstream)
///
/// After this setup:
/// - upstream has 2 commits
/// - local has 1 commit (behind by 1)
/// - local can `git pull` to get the second commit
fn setup_pull_test() -> PullTestSetup {
    let (local, upstream) = TestRepo::new_with_remote();

    // Make initial commit in local and push
    let mut readme = local.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    let commit = local
        .stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    let initial_sha = commit.commit_sha;

    // Push initial commit to upstream
    local
        .git(&["push", "-u", "origin", "HEAD"])
        .expect("push initial commit should succeed");

    // Make second commit (simulating remote changes)
    let mut file = local.filename("upstream_file.txt");
    file.set_contents(vec!["content from upstream".to_string()]);
    let commit = local
        .stage_all_and_commit("upstream commit")
        .expect("upstream commit should succeed");

    let upstream_sha = commit.commit_sha;

    // Push second commit to upstream
    local
        .git(&["push", "origin", "HEAD"])
        .expect("push upstream commit should succeed");

    // Reset local back to initial commit (so it's behind upstream)
    local
        .git(&["reset", "--hard", &initial_sha])
        .expect("reset to initial commit should succeed");

    // Verify local is behind
    assert!(
        local.read_file("upstream_file.txt").is_none(),
        "Local should not have upstream_file.txt after reset"
    );

    PullTestSetup {
        local,
        upstream,
        upstream_sha,
    }
}

#[test]
fn test_fast_forward_pull_preserves_ai_attribution() {
    let setup = setup_pull_test();
    let local = setup.local;

    // Create local AI changes (uncommitted)
    let mut ai_file = local.filename("ai_work.txt");
    ai_file.set_contents(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);

    local
        .git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Perform fast-forward pull
    local.git(&["pull"]).expect("pull should succeed");

    // Commit and verify AI attribution is preserved through the ff pull
    local
        .stage_all_and_commit("commit after pull")
        .expect("commit should succeed");

    ai_file.assert_lines_and_blame(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);
}

#[test]
fn test_pull_rebase_autostash_preserves_ai_attribution() {
    let setup = setup_pull_test();
    let local = setup.local;

    // Create local AI changes (uncommitted)
    let mut ai_file = local.filename("ai_work.txt");
    ai_file.set_contents(vec![
        "AI generated line 1".ai(),
        "AI generated line 2".ai(),
        "AI generated line 3".ai(),
    ]);

    local
        .git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Perform pull with --rebase --autostash flags
    local
        .git(&["pull", "--rebase", "--autostash"])
        .expect("pull --rebase --autostash should succeed");

    // Commit and verify AI attribution is preserved through stash/unstash cycle
    local
        .stage_all_and_commit("commit after rebase pull")
        .expect("commit should succeed");

    ai_file.assert_lines_and_blame(vec![
        "AI generated line 1".ai(),
        "AI generated line 2".ai(),
        "AI generated line 3".ai(),
    ]);
}

#[test]
fn test_pull_rebase_autostash_with_mixed_attribution() {
    let setup = setup_pull_test();
    let local = setup.local;

    // Create local changes with mixed human and AI attribution
    let mut mixed_file = local.filename("mixed_work.txt");
    mixed_file.set_contents(vec![
        "Human written line 1".human(),
        "AI generated line 1".ai(),
        "Human written line 2".human(),
        "AI generated line 2".ai(),
    ]);

    local
        .git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Perform pull with --rebase --autostash
    local
        .git(&["pull", "--rebase", "--autostash"])
        .expect("pull --rebase --autostash should succeed");

    // Commit and verify mixed attribution is preserved
    local
        .stage_all_and_commit("commit with mixed attribution")
        .expect("commit should succeed");

    mixed_file.assert_lines_and_blame(vec![
        "Human written line 1".human(),
        "AI generated line 1".ai(),
        "Human written line 2".human(),
        "AI generated line 2".ai(),
    ]);
}

#[test]
fn test_pull_rebase_autostash_via_git_config() {
    let setup = setup_pull_test();
    let local = setup.local;

    // Set git config to always use rebase and autostash for pull
    local
        .git(&["config", "pull.rebase", "true"])
        .expect("set pull.rebase should succeed");
    local
        .git(&["config", "rebase.autoStash", "true"])
        .expect("set rebase.autoStash should succeed");

    // Create local AI changes (uncommitted)
    let mut ai_file = local.filename("ai_config_test.txt");
    ai_file.set_contents(vec!["AI line via config".ai()]);

    local
        .git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Perform regular pull (should use rebase+autostash from config)
    local.git(&["pull"]).expect("pull should succeed");

    // Commit and verify AI attribution is preserved
    local
        .stage_all_and_commit("commit after config-based rebase pull")
        .expect("commit should succeed");

    ai_file.assert_lines_and_blame(vec!["AI line via config".ai()]);
}

#[test]
fn test_fast_forward_pull_without_local_changes() {
    let setup = setup_pull_test();
    let local = setup.local;

    // No local changes - just a clean fast-forward pull

    // Perform fast-forward pull
    local.git(&["pull"]).expect("pull should succeed");

    // Verify we got the upstream changes
    assert!(
        local.read_file("upstream_file.txt").is_some(),
        "Should have upstream_file.txt after pull"
    );

    // Verify HEAD is at the expected upstream commit
    let head = local.git(&["rev-parse", "HEAD"]).unwrap();
    assert_eq!(
        head.trim(),
        setup.upstream_sha,
        "HEAD should be at upstream commit"
    );
}
