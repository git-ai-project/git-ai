mod repos;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

/// Helper struct that provides a local repo with an upstream containing seeded commits.
/// The local repo is initially behind the upstream (no divergence — fast-forward possible).
struct PullTestSetup {
    /// The local clone - initially behind upstream after setup
    local: TestRepo,
    /// The bare upstream repository (kept alive for the duration of the test)
    #[allow(dead_code)]
    upstream: TestRepo,
    /// SHA of the second commit (upstream is ahead by this)
    upstream_sha: String,
}

/// Creates a test setup for fast-forward pull scenarios:
/// 1. Creates upstream (bare) and local (clone) repos
/// 2. Makes an initial commit in local, pushes to upstream
/// 3. Makes a second commit in local, pushes to upstream
/// 4. Resets local back to initial commit (so local is behind upstream)
///
/// After this setup:
/// - upstream has 2 commits
/// - local has 1 commit (behind by 1)
/// - local can `git pull` to fast-forward to the second commit
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

/// Helper struct for divergent pull scenarios where local has committed changes
/// and upstream has diverged, requiring a real rebase (not fast-forward).
struct DivergentPullTestSetup {
    local: TestRepo,
    #[allow(dead_code)]
    upstream: TestRepo,
    /// SHA of the local AI commit (will get a new SHA after rebase)
    local_ai_commit_sha: String,
}

/// Creates a test setup for divergent pull --rebase scenarios:
/// 1. Creates upstream (bare) and local (clone) repos
/// 2. Makes an initial commit, pushes to upstream
/// 3. Makes a local AI-authored commit
/// 4. Creates a divergent upstream commit (force-pushed)
/// 5. Resets local back to the AI commit
///
/// After this setup:
/// - upstream has diverged from local (initial + upstream_commit)
/// - local has diverged from upstream (initial + ai_commit)
/// - `git pull --rebase` will rebase the AI commit onto the upstream commit
fn setup_divergent_pull_test() -> DivergentPullTestSetup {
    let (local, upstream) = TestRepo::new_with_remote();

    // Make initial commit and push
    let mut readme = local.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    let initial = local
        .stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    local
        .git(&["push", "-u", "origin", "HEAD"])
        .expect("push initial commit should succeed");

    // Create a local committed AI-authored change
    let mut ai_file = local.filename("ai_feature.txt");
    ai_file.set_contents(vec![
        "AI generated feature line 1".ai(),
        "AI generated feature line 2".ai(),
    ]);
    local
        .stage_all_and_commit("add AI feature")
        .expect("AI feature commit should succeed");

    let ai_commit_sha = local
        .git(&["rev-parse", "HEAD"])
        .expect("rev-parse should succeed")
        .trim()
        .to_string();

    let branch = local.current_branch();

    // Create a divergent upstream commit: reset to initial, commit, force-push
    local
        .git(&["reset", "--hard", &initial.commit_sha])
        .expect("reset should succeed");

    let mut upstream_file = local.filename("upstream_change.txt");
    upstream_file.set_contents(vec!["upstream content".to_string()]);
    local
        .stage_all_and_commit("upstream divergent commit")
        .expect("upstream commit should succeed");

    local
        .git(&["push", "--force", "origin", &format!("HEAD:{}", branch)])
        .expect("force push upstream commit should succeed");

    // Reset back to the local AI commit
    local
        .git(&["reset", "--hard", &ai_commit_sha])
        .expect("reset to AI commit should succeed");

    DivergentPullTestSetup {
        local,
        upstream,
        local_ai_commit_sha: ai_commit_sha,
    }
}

/// Creates a setup where local has one AI commit and upstream has an equivalent patch
/// under a different commit hash plus additional upstream commits.
/// A subsequent `pull --rebase` should skip the local commit and not map all upstream history
/// as "new rebased commits".
fn setup_pull_rebase_skip_test() -> (TestRepo, TestRepo, String) {
    let (local, upstream) = TestRepo::new_with_remote();

    // Initial commit and push
    let mut readme = local.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    let initial = local
        .stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");
    local
        .git(&["push", "-u", "origin", "HEAD"])
        .expect("push initial commit should succeed");

    // Local AI commit (this is the one that should be skipped during pull --rebase)
    let mut ai_file = local.filename("ai_feature.txt");
    ai_file.set_contents(vec![
        "AI generated feature line 1".ai(),
        "AI generated feature line 2".ai(),
    ]);
    let local_ai = local
        .stage_all_and_commit("local ai commit")
        .expect("local ai commit should succeed");

    let branch = local.current_branch();

    // Simulate upstream history with equivalent patch under a different commit hash.
    // Reset to initial, re-commit same file content with different message, then add extra commits.
    local
        .git(&["reset", "--hard", &initial.commit_sha])
        .expect("reset to initial should succeed");

    ai_file.set_contents(vec![
        "AI generated feature line 1".ai(),
        "AI generated feature line 2".ai(),
    ]);
    local
        .stage_all_and_commit("upstream equivalent ai commit")
        .expect("upstream equivalent ai commit should succeed");

    let mut upstream_file = local.filename("upstream_only.txt");
    upstream_file.set_contents(vec!["upstream extra 1".to_string()]);
    local
        .stage_all_and_commit("upstream extra 1")
        .expect("upstream extra 1 should succeed");
    upstream_file.set_contents(vec![
        "upstream extra 1".to_string(),
        "upstream extra 2".to_string(),
    ]);
    local
        .stage_all_and_commit("upstream extra 2")
        .expect("upstream extra 2 should succeed");

    // Force-push divergent upstream state
    local
        .git(&["push", "--force", "origin", &format!("HEAD:{}", branch)])
        .expect("force push upstream state should succeed");

    // Restore local branch to the original local AI commit (now divergent from upstream)
    local
        .git(&["reset", "--hard", &local_ai.commit_sha])
        .expect("reset back to local ai commit should succeed");

    (local, upstream, local_ai.commit_sha)
}

// =============================================================================
// Fast-forward pull tests
// =============================================================================

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
fn test_fast_forward_pull_without_local_changes() {
    let setup = setup_pull_test();
    let local = setup.local;

    // No local changes - just a clean fast-forward pull
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

// =============================================================================
// Pull --rebase with committed changes (the core bug fix)
// =============================================================================

#[test]
fn test_pull_rebase_preserves_committed_ai_authorship() {
    let setup = setup_divergent_pull_test();
    let local = setup.local;

    // Perform pull --rebase (committed local changes will be rebased onto upstream)
    local
        .git(&["pull", "--rebase"])
        .expect("pull --rebase should succeed");

    // Verify we got upstream changes
    assert!(
        local.read_file("upstream_change.txt").is_some(),
        "Should have upstream_change.txt after pull --rebase"
    );

    // The AI commit got a new SHA after rebase
    let new_head = local
        .git(&["rev-parse", "HEAD"])
        .expect("rev-parse should succeed")
        .trim()
        .to_string();

    assert_ne!(
        new_head, setup.local_ai_commit_sha,
        "HEAD should have a new SHA after rebase"
    );

    // Verify AI authorship is preserved on the rebased commit
    let mut ai_file = local.filename("ai_feature.txt");
    ai_file.assert_lines_and_blame(vec![
        "AI generated feature line 1".ai(),
        "AI generated feature line 2".ai(),
    ]);
}

#[test]
fn test_pull_rebase_via_git_config_preserves_committed_ai_authorship() {
    let setup = setup_divergent_pull_test();
    let local = setup.local;

    // Set git config to use rebase for pull (no --rebase flag needed)
    local
        .git(&["config", "pull.rebase", "true"])
        .expect("set pull.rebase should succeed");

    // Perform plain pull (should rebase due to config)
    local.git(&["pull"]).expect("pull should succeed");

    // Verify upstream changes arrived and commit SHA changed
    assert!(
        local.read_file("upstream_change.txt").is_some(),
        "Should have upstream_change.txt after pull"
    );

    let new_head = local
        .git(&["rev-parse", "HEAD"])
        .expect("rev-parse should succeed")
        .trim()
        .to_string();

    assert_ne!(
        new_head, setup.local_ai_commit_sha,
        "HEAD should have a new SHA after rebase"
    );

    // Verify AI authorship survived
    let mut ai_file = local.filename("ai_feature.txt");
    ai_file.assert_lines_and_blame(vec![
        "AI generated feature line 1".ai(),
        "AI generated feature line 2".ai(),
    ]);
}

// =============================================================================
// Pull --rebase --autostash with uncommitted changes
// =============================================================================

#[test]
fn test_pull_rebase_autostash_preserves_uncommitted_ai_attribution() {
    let setup = setup_divergent_pull_test();
    let local = setup.local;

    // Add uncommitted AI changes on top of the committed ones
    let mut uncommitted_ai = local.filename("uncommitted_ai.txt");
    uncommitted_ai.set_contents(vec![
        "AI generated line 1".ai(),
        "AI generated line 2".ai(),
        "AI generated line 3".ai(),
    ]);

    local
        .git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Pull --rebase --autostash: uncommitted changes get stashed/unstashed
    local
        .git(&["pull", "--rebase", "--autostash"])
        .expect("pull --rebase --autostash should succeed");

    // Commit the previously-uncommitted changes
    local
        .stage_all_and_commit("commit after rebase pull")
        .expect("commit should succeed");

    uncommitted_ai.assert_lines_and_blame(vec![
        "AI generated line 1".ai(),
        "AI generated line 2".ai(),
        "AI generated line 3".ai(),
    ]);
}

#[test]
fn test_pull_rebase_autostash_with_mixed_attribution() {
    let setup = setup_divergent_pull_test();
    let local = setup.local;

    // Create local uncommitted changes with mixed human and AI attribution
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

    // Pull --rebase --autostash
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

    // Create local uncommitted AI changes
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

// =============================================================================
// Pull --rebase with both committed AND uncommitted changes
// =============================================================================

#[test]
fn test_pull_rebase_committed_and_autostash_preserves_all_authorship() {
    let setup = setup_divergent_pull_test();
    let local = setup.local;

    // Add uncommitted AI changes on top of the committed AI commit
    let mut uncommitted_ai = local.filename("uncommitted_ai.txt");
    uncommitted_ai.set_contents(vec!["Uncommitted AI line".ai()]);
    local
        .git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Pull --rebase --autostash: committed changes get rebased, uncommitted get stashed
    local
        .git(&["pull", "--rebase", "--autostash"])
        .expect("pull --rebase --autostash should succeed");

    // Commit the previously-uncommitted changes
    local
        .stage_all_and_commit("commit uncommitted AI work")
        .expect("commit should succeed");

    // Verify committed AI authorship survived the rebase
    let mut committed_ai = local.filename("ai_feature.txt");
    committed_ai.assert_lines_and_blame(vec![
        "AI generated feature line 1".ai(),
        "AI generated feature line 2".ai(),
    ]);

    // Verify uncommitted AI authorship survived the autostash cycle
    uncommitted_ai.assert_lines_and_blame(vec!["Uncommitted AI line".ai()]);
}

#[test]
fn test_pull_rebase_skip_commit_does_not_map_entire_upstream_history() {
    let (local, _upstream, local_ai_sha) = setup_pull_rebase_skip_test();

    let output = local
        .git(&["pull", "--rebase"])
        .expect("pull --rebase should succeed");

    // Local commit was duplicated upstream via equivalent patch, so rebase should skip it.
    // We expect no newly rebased commits to map, rather than traversing all upstream commits.
    assert!(
        output.contains("Commit mapping: 1 original -> 0 new"),
        "Expected skipped-commit pull --rebase mapping to be 1 original -> 0 new. Output:\n{}",
        output
    );

    // HEAD should move away from original local commit onto upstream tip.
    let new_head = local
        .git(&["rev-parse", "HEAD"])
        .expect("rev-parse should succeed")
        .trim()
        .to_string();
    assert_ne!(
        new_head, local_ai_sha,
        "HEAD should have moved to upstream history after skipped rebase"
    );
}

#[test]
fn test_failed_pull_rebase_without_autostash_does_not_leak_stale_ai_metadata() {
    let setup = setup_divergent_pull_test();
    let local = setup.local;

    // Create uncommitted AI changes, then run pull --rebase without autostash.
    let mut dirty = local.filename("dirty.txt");
    dirty.set_contents(vec!["stale ai line".ai()]);
    local
        .git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    let failed_pull = local.git(&["pull", "--rebase"]);
    assert!(failed_pull.is_err(), "dirty pull --rebase should fail");

    // Drop uncommitted changes and complete a clean pull --rebase.
    local
        .git(&["reset", "--hard", "HEAD"])
        .expect("hard reset should succeed");
    local
        .git(&["pull", "--rebase"])
        .expect("clean pull --rebase should succeed");

    // A later human-only commit must not inherit stale AI prompt metadata.
    let mut human_only = local.filename("human.txt");
    human_only.set_contents(vec!["human only".human()]);
    let final_commit = local
        .stage_all_and_commit("human follow-up")
        .expect("human follow-up commit should succeed");
    assert!(
        final_commit.authorship_log.metadata.prompts.is_empty(),
        "stale pull-autostash attribution leaked into later human-only commit"
    );
}
