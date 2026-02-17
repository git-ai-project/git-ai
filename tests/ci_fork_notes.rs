#[macro_use]
mod repos;
use git_ai::ci::ci_context::{CiContext, CiEvent, CiRunResult};
use git_ai::git::refs::get_reference_as_authorship_log_v3;
use git_ai::git::repository as GitAiRepository;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

/// Helper: set up "origin" as a self-referencing remote so fetch_authorship_notes("origin")
/// doesn't fail. In real CI the repo is cloned from origin, so it always exists.
fn add_self_origin(repo: &TestRepo) {
    let path = repo.path().to_str().unwrap();
    repo.git_og(&["remote", "add", "origin", path]).ok(); // ok() in case it already exists
}

/// Test that CI preserves fork notes for a squash merge from a fork.
///
/// Scenario:
/// 1. Contributor works in a fork with git-ai, creating AI-attributed code
/// 2. Maintainer squash-merges the PR into the upstream repo
/// 3. CI runs and should fetch notes from the fork, then rewrite them
///    onto the squash commit
#[test]
fn test_ci_fork_squash_merge() {
    // Setup: create "upstream" repo with initial commit
    let upstream = TestRepo::new();
    let mut file = upstream.filename("feature.js");

    file.set_contents(lines!["// Original code", "function original() {}"]);
    let base_commit = upstream.stage_all_and_commit("Initial commit").unwrap();
    upstream.git(&["branch", "-M", "main"]).unwrap();
    add_self_origin(&upstream);

    // Create "fork" repo and give it the upstream's history
    let fork = TestRepo::new();
    let upstream_path = upstream.path().to_str().unwrap().to_string();
    fork.git_og(&["remote", "add", "upstream", &upstream_path])
        .unwrap();
    fork.git_og(&["fetch", "upstream"]).unwrap();
    fork.git_og(&["checkout", "-b", "main", "upstream/main"])
        .unwrap();

    // Fork contributor adds AI code using git-ai
    let mut fork_file = fork.filename("feature.js");
    fork_file.set_contents(lines![
        "// Original code",
        "function original() {}",
        "// AI added function".ai(),
        "function aiFeature() {".ai(),
        "  return 'from fork';".ai(),
        "}".ai()
    ]);
    let fork_commit = fork.stage_all_and_commit("Add AI feature in fork").unwrap();
    let fork_head_sha = fork_commit.commit_sha.clone();

    // Verify fork has authorship notes
    let fork_repo = GitAiRepository::find_repository_in_path(fork.path().to_str().unwrap())
        .expect("Failed to find fork repository");
    assert!(
        get_reference_as_authorship_log_v3(&fork_repo, &fork_head_sha).is_ok(),
        "Fork commit should have authorship notes"
    );

    // Make the fork's commits accessible from upstream
    upstream
        .git_og(&["remote", "add", "fork", fork.path().to_str().unwrap()])
        .unwrap();
    upstream
        .git_og(&["fetch", "fork", "main:refs/fork/main"])
        .unwrap();

    // Simulate squash merge in upstream: maintainer creates a squash commit.
    // Use git_og (raw git) to avoid git-ai auto-creating notes on this commit,
    // which simulates the real CI scenario where the squash commit has no notes.
    file.set_contents(lines![
        "// Original code",
        "function original() {}",
        "// AI added function",
        "function aiFeature() {",
        "  return 'from fork';",
        "}"
    ]);
    upstream.git_og(&["add", "-A"]).unwrap();
    upstream
        .git_og(&["commit", "-m", "Merge fork PR via squash (#1)"])
        .unwrap();
    let merge_sha = upstream
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Run CI context with fork_clone_url set
    let upstream_repo = GitAiRepository::find_repository_in_path(upstream.path().to_str().unwrap())
        .expect("Failed to find upstream repository");

    let fork_url = fork.path().to_str().unwrap().to_string();

    let ci_context = CiContext::with_repository(
        upstream_repo,
        CiEvent::Merge {
            merge_commit_sha: merge_sha.clone(),
            head_ref: "main".to_string(),
            head_sha: fork_head_sha.clone(),
            base_ref: "main".to_string(),
            base_sha: base_commit.commit_sha.clone(),
            fork_clone_url: Some(fork_url),
        },
    );

    let result = ci_context.run().unwrap();

    // Verify the result is AuthorshipRewritten (squash merge rewrites notes)
    assert!(
        matches!(result, CiRunResult::AuthorshipRewritten { .. }),
        "Expected AuthorshipRewritten for fork squash merge, got {:?}",
        result
    );

    // Verify authorship is preserved in the squash commit
    file.assert_lines_and_blame(lines![
        "// Original code".human(),
        "function original() {}".human(),
        "// AI added function".ai(),
        "function aiFeature() {".ai(),
        "  return 'from fork';".ai(),
        "}".ai()
    ]);
}

/// Test that CI preserves fork notes for a merge commit (non-squash, non-rebase).
///
/// For merge commits from forks, the merged commits keep their original SHAs.
/// The CI should fetch notes from the fork and push them to origin.
#[test]
fn test_ci_fork_merge_commit() {
    // Setup: create "upstream" repo with initial commit
    let upstream = TestRepo::new();
    let mut file = upstream.filename("feature.js");

    file.set_contents(lines!["// Original code", "function original() {}"]);
    let base_commit = upstream.stage_all_and_commit("Initial commit").unwrap();
    upstream.git(&["branch", "-M", "main"]).unwrap();
    add_self_origin(&upstream);

    // Create "fork" as separate repo with shared history
    let fork = TestRepo::new();
    let upstream_path = upstream.path().to_str().unwrap().to_string();
    fork.git_og(&["remote", "add", "upstream", &upstream_path])
        .unwrap();
    fork.git_og(&["fetch", "upstream"]).unwrap();
    fork.git_og(&["checkout", "-b", "main", "upstream/main"])
        .unwrap();

    // Fork contributor adds AI code
    let mut fork_file = fork.filename("feature.js");
    fork_file.set_contents(lines![
        "// Original code",
        "function original() {}",
        "// AI feature from fork".ai(),
        "function forkFeature() {".ai(),
        "  return true;".ai(),
        "}".ai()
    ]);
    let fork_commit = fork.stage_all_and_commit("Add AI feature in fork").unwrap();
    let fork_head_sha = fork_commit.commit_sha.clone();

    // Verify fork has authorship notes
    let fork_repo = GitAiRepository::find_repository_in_path(fork.path().to_str().unwrap())
        .expect("Failed to find fork repository");
    assert!(
        get_reference_as_authorship_log_v3(&fork_repo, &fork_head_sha).is_ok(),
        "Fork commit should have authorship notes"
    );

    // Fetch fork commits and create merge commit in upstream
    upstream
        .git_og(&["remote", "add", "fork", fork.path().to_str().unwrap()])
        .unwrap();
    upstream
        .git_og(&["fetch", "fork", "main:refs/fork/main"])
        .unwrap();

    // Create a merge commit (--no-ff ensures a merge commit is created)
    upstream
        .git_og(&[
            "merge",
            "--no-ff",
            "refs/fork/main",
            "-m",
            "Merge fork PR (#1)",
        ])
        .unwrap();
    let merge_sha = upstream
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Run CI context with fork_clone_url
    let upstream_repo = GitAiRepository::find_repository_in_path(upstream.path().to_str().unwrap())
        .expect("Failed to find upstream repository");

    let fork_url = fork.path().to_str().unwrap().to_string();

    let ci_context = CiContext::with_repository(
        upstream_repo,
        CiEvent::Merge {
            merge_commit_sha: merge_sha.clone(),
            head_ref: "main".to_string(),
            head_sha: fork_head_sha.clone(),
            base_ref: "main".to_string(),
            base_sha: base_commit.commit_sha.clone(),
            fork_clone_url: Some(fork_url),
        },
    );

    let result = ci_context.run().unwrap();

    // For merge commits from forks, notes should be preserved (not rewritten)
    assert!(
        matches!(result, CiRunResult::ForkNotesPreserved),
        "Expected ForkNotesPreserved for fork merge commit, got {:?}",
        result
    );

    // Verify the fork commit's authorship is accessible in upstream
    let upstream_repo2 =
        GitAiRepository::find_repository_in_path(upstream.path().to_str().unwrap())
            .expect("Failed to find upstream repository");
    let authorship_log = get_reference_as_authorship_log_v3(&upstream_repo2, &fork_head_sha);
    assert!(
        authorship_log.is_ok(),
        "Fork commit's authorship should be accessible in upstream after CI run"
    );
}

/// Test that CI handles fork PRs with no notes gracefully.
///
/// If a fork contributor doesn't use git-ai, there are no notes to fetch.
/// The CI should handle this gracefully without errors.
#[test]
fn test_ci_fork_no_notes() {
    // Setup upstream
    let upstream = TestRepo::new();
    let mut file = upstream.filename("feature.js");

    file.set_contents(lines!["// Original code"]);
    let base_commit = upstream.stage_all_and_commit("Initial commit").unwrap();
    upstream.git(&["branch", "-M", "main"]).unwrap();
    add_self_origin(&upstream);

    // Create fork WITHOUT git-ai (using git_og for all operations)
    let fork = TestRepo::new();
    let upstream_path = upstream.path().to_str().unwrap().to_string();
    fork.git_og(&["remote", "add", "upstream", &upstream_path])
        .unwrap();
    fork.git_og(&["fetch", "upstream"]).unwrap();
    fork.git_og(&["checkout", "-b", "main", "upstream/main"])
        .unwrap();

    // Fork contributor adds code without git-ai
    let mut fork_file = fork.filename("feature.js");
    fork_file.set_contents(lines!["// Original code", "// Added in fork"]);
    fork.git_og(&["add", "-A"]).unwrap();
    fork.git_og(&["commit", "-m", "Add feature in fork"])
        .unwrap();
    let fork_head_sha = fork
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Fetch fork commits into upstream
    upstream
        .git_og(&["remote", "add", "fork", fork.path().to_str().unwrap()])
        .unwrap();
    upstream
        .git_og(&["fetch", "fork", "main:refs/fork/main"])
        .unwrap();

    // Simulate squash merge in upstream (using raw git to avoid auto-notes)
    file.set_contents(lines!["// Original code", "// Added in fork"]);
    upstream.git_og(&["add", "-A"]).unwrap();
    upstream
        .git_og(&["commit", "-m", "Merge fork PR via squash"])
        .unwrap();
    let merge_sha = upstream
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Run CI with fork URL
    let upstream_repo = GitAiRepository::find_repository_in_path(upstream.path().to_str().unwrap())
        .expect("Failed to find upstream repository");

    let fork_url = fork.path().to_str().unwrap().to_string();

    let ci_context = CiContext::with_repository(
        upstream_repo,
        CiEvent::Merge {
            merge_commit_sha: merge_sha.clone(),
            head_ref: "main".to_string(),
            head_sha: fork_head_sha,
            base_ref: "main".to_string(),
            base_sha: base_commit.commit_sha.clone(),
            fork_clone_url: Some(fork_url),
        },
    );

    // Should complete without errors, even though fork has no notes
    let result = ci_context.run().unwrap();
    assert!(
        matches!(result, CiRunResult::NoAuthorshipAvailable),
        "Expected NoAuthorshipAvailable for fork with no git-ai notes, got {:?}",
        result
    );
}

/// Test that non-fork PRs (fork_clone_url = None) still work as before.
/// Merge commits without fork_clone_url should still be skipped.
#[test]
fn test_ci_non_fork_merge_commit_still_skipped() {
    let upstream = TestRepo::new();
    let mut file = upstream.filename("feature.js");

    file.set_contents(lines!["// Original code"]);
    let base_commit = upstream.stage_all_and_commit("Initial commit").unwrap();
    upstream.git(&["branch", "-M", "main"]).unwrap();
    add_self_origin(&upstream);

    // Create feature branch (same repo, not a fork)
    upstream.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = upstream.filename("feature.js");
    feature_file.set_contents(lines![
        "// Original code",
        "// Feature addition".ai(),
        "function feature() {}".ai()
    ]);
    let feature_commit = upstream.stage_all_and_commit("Add feature").unwrap();
    let feature_sha = feature_commit.commit_sha.clone();

    // Merge with --no-ff to create merge commit
    upstream.git(&["checkout", "main"]).unwrap();
    upstream
        .git_og(&["merge", "--no-ff", "feature", "-m", "Merge feature"])
        .unwrap();
    let merge_sha = upstream
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let upstream_repo = GitAiRepository::find_repository_in_path(upstream.path().to_str().unwrap())
        .expect("Failed to find repository");

    let ci_context = CiContext::with_repository(
        upstream_repo,
        CiEvent::Merge {
            merge_commit_sha: merge_sha,
            head_ref: "feature".to_string(),
            head_sha: feature_sha,
            base_ref: "main".to_string(),
            base_sha: base_commit.commit_sha.clone(),
            fork_clone_url: None, // Not a fork
        },
    );

    let result = ci_context.run().unwrap();

    // Should still be SkippedSimpleMerge for non-fork merge commits
    assert!(
        matches!(result, CiRunResult::SkippedSimpleMerge),
        "Expected SkippedSimpleMerge for non-fork merge commit, got {:?}",
        result
    );
}

/// Test squash merge from fork with multiple commits containing AI code.
/// Verify that authorship is rewritten (fork notes are fetched and processed).
#[test]
fn test_ci_fork_squash_merge_multiple_commits() {
    let upstream = TestRepo::new();
    let mut file = upstream.filename("app.js");

    file.set_contents(lines!["// App v1", ""]);
    let base_commit = upstream.stage_all_and_commit("Initial commit").unwrap();
    upstream.git(&["branch", "-M", "main"]).unwrap();
    add_self_origin(&upstream);

    // Create fork with multiple AI commits
    let fork = TestRepo::new();
    let upstream_path = upstream.path().to_str().unwrap().to_string();
    fork.git_og(&["remote", "add", "upstream", &upstream_path])
        .unwrap();
    fork.git_og(&["fetch", "upstream"]).unwrap();
    fork.git_og(&["checkout", "-b", "main", "upstream/main"])
        .unwrap();

    let mut fork_file = fork.filename("app.js");

    // First commit: AI adds function 1
    fork_file.insert_at(
        1,
        lines!["// AI function 1".ai(), "function ai1() { }".ai()],
    );
    fork.stage_all_and_commit("Add AI function 1").unwrap();

    // Second commit: AI adds function 2
    fork_file.insert_at(
        3,
        lines!["// AI function 2".ai(), "function ai2() { }".ai()],
    );
    fork.stage_all_and_commit("Add AI function 2").unwrap();

    // Third commit: Human adds function
    fork_file.insert_at(5, lines!["// Human function", "function human() { }"]);
    let fork_last_commit = fork.stage_all_and_commit("Add human function").unwrap();
    let fork_head_sha = fork_last_commit.commit_sha.clone();

    // Fetch fork commits into upstream
    upstream
        .git_og(&["remote", "add", "fork", fork.path().to_str().unwrap()])
        .unwrap();
    upstream
        .git_og(&["fetch", "fork", "main:refs/fork/main"])
        .unwrap();

    // Simulate squash merge in upstream (using raw git to avoid auto-notes)
    file.set_contents(lines![
        "// App v1",
        "// AI function 1",
        "function ai1() { }",
        "// AI function 2",
        "function ai2() { }",
        "// Human function",
        "function human() { }"
    ]);
    upstream.git_og(&["add", "-A"]).unwrap();
    upstream
        .git_og(&["commit", "-m", "Merge fork multi-commit PR (#2)"])
        .unwrap();
    let merge_sha = upstream
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Run CI with fork URL
    let upstream_repo = GitAiRepository::find_repository_in_path(upstream.path().to_str().unwrap())
        .expect("Failed to find upstream repository");

    let fork_url = fork.path().to_str().unwrap().to_string();

    let ci_context = CiContext::with_repository(
        upstream_repo,
        CiEvent::Merge {
            merge_commit_sha: merge_sha.clone(),
            head_ref: "main".to_string(),
            head_sha: fork_head_sha,
            base_ref: "main".to_string(),
            base_sha: base_commit.commit_sha.clone(),
            fork_clone_url: Some(fork_url),
        },
    );

    let result = ci_context.run().unwrap();

    // Verify fork notes were fetched and authorship was rewritten
    assert!(
        matches!(result, CiRunResult::AuthorshipRewritten { .. }),
        "Expected AuthorshipRewritten for multi-commit fork squash merge, got {:?}",
        result
    );

    // Verify authorship log exists on the merge commit
    let upstream_repo2 =
        GitAiRepository::find_repository_in_path(upstream.path().to_str().unwrap())
            .expect("Failed to find upstream repository");
    let authorship_log = get_reference_as_authorship_log_v3(&upstream_repo2, &merge_sha);
    assert!(
        authorship_log.is_ok(),
        "Squash commit should have authorship log from fork notes"
    );
    let log = authorship_log.unwrap();
    assert!(
        !log.attestations.is_empty(),
        "Authorship log should have attestations from fork's AI code"
    );
}
