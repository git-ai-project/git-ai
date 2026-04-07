use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{GitTestMode, TestRepo};
use git_ai::git::refs::get_reference_as_authorship_log_v3;
use git_ai::git::repository as GitAiRepository;

fn direct_test_repo() -> TestRepo {
    TestRepo::new_with_mode(GitTestMode::Wrapper)
}

/// Test basic squash merge via CI - AI code from feature branch squashed into main
#[test]
fn test_ci_squash_merge_basic() {
    let repo = direct_test_repo();
    let mut file = repo.filename("feature.js");

    // Create initial commit on main (rename default branch to main)
    file.set_contents(crate::lines!["// Original code", "function original() {}"]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with AI code
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(
        2,
        crate::lines![
            "// AI added function".ai(),
            "function aiFeature() {".ai(),
            "  return 'ai code';".ai(),
            "}".ai()
        ],
    );
    let feature_commit = repo.stage_all_and_commit("Add AI feature").unwrap();
    let feature_sha = feature_commit.commit_sha;

    // Simulate CI squash merge: checkout main, create merge commit
    repo.git(&["checkout", "main"]).unwrap();

    // Manually create the squashed state (as CI would do)
    file.set_contents(crate::lines![
        "// Original code",
        "function original() {}",
        "// AI added function",
        "function aiFeature() {",
        "  return 'ai code';",
        "}"
    ]);
    let merge_commit = repo
        .stage_all_and_commit("Merge feature via squash")
        .unwrap();
    let merge_sha = merge_commit.commit_sha;

    // Get the GitAi repository instance
    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    // Call the CI rewrite function
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;
    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feature",
        "main",
        &feature_sha,
        &merge_sha,
        false,
    )
    .unwrap();

    // Verify AI authorship is preserved in the merge commit
    file.assert_lines_and_blame(crate::lines![
        "// Original code".human(),
        "function original() {}".ai(),
        "// AI added function".ai(),
        "function aiFeature() {".ai(),
        "  return 'ai code';".ai(),
        "}".ai()
    ]);
}

/// Test squash merge with multiple files containing AI code
#[test]
fn test_ci_squash_merge_multiple_files() {
    let repo = direct_test_repo();

    // Create initial commit on main with two files
    let mut file1 = repo.filename("file1.js");
    let mut file2 = repo.filename("file2.js");

    file1.set_contents(crate::lines!["// File 1 original"]);
    file2.set_contents(crate::lines!["// File 2 original"]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with AI changes to both files
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    file1.insert_at(
        1,
        crate::lines!["// AI code in file1".ai(), "const feature1 = 'ai';".ai()],
    );
    file2.insert_at(
        1,
        crate::lines!["// AI code in file2".ai(), "const feature2 = 'ai';".ai()],
    );

    let feature_commit = repo
        .stage_all_and_commit("Add AI features to both files")
        .unwrap();
    let feature_sha = feature_commit.commit_sha;

    // Simulate CI squash merge
    repo.git(&["checkout", "main"]).unwrap();

    file1.set_contents(crate::lines![
        "// File 1 original",
        "// AI code in file1",
        "const feature1 = 'ai';"
    ]);
    file2.set_contents(crate::lines![
        "// File 2 original",
        "// AI code in file2",
        "const feature2 = 'ai';"
    ]);

    let merge_commit = repo
        .stage_all_and_commit("Merge feature via squash")
        .unwrap();
    let merge_sha = merge_commit.commit_sha;

    // Get the GitAi repository instance
    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    // Call the CI rewrite function
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;
    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feature",
        "main",
        &feature_sha,
        &merge_sha,
        false,
    )
    .unwrap();

    // Verify AI authorship is preserved in both files
    file1.assert_lines_and_blame(crate::lines![
        "// File 1 original".ai(),
        "// AI code in file1".ai(),
        "const feature1 = 'ai';".ai()
    ]);

    file2.assert_lines_and_blame(crate::lines![
        "// File 2 original".ai(),
        "// AI code in file2".ai(),
        "const feature2 = 'ai';".ai()
    ]);
}

/// Test squash merge with mixed AI and human content
#[test]
fn test_ci_squash_merge_mixed_content() {
    let repo = direct_test_repo();
    let mut file = repo.filename("mixed.js");

    // Create initial commit
    file.set_contents(crate::lines!["// Base code", "const base = 1;"]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with mixed AI and human changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Simulate: human adds a comment, AI adds code, human adds more
    file.insert_at(
        2,
        crate::lines![
            "// Human comment",
            "// AI generated function".ai(),
            "function aiHelper() {".ai(),
            "  return true;".ai(),
            "}".ai(),
            "// Another human comment"
        ],
    );

    let feature_commit = repo.stage_all_and_commit("Add mixed content").unwrap();
    let feature_sha = feature_commit.commit_sha;

    // Simulate CI squash merge
    repo.git(&["checkout", "main"]).unwrap();

    file.set_contents(crate::lines![
        "// Base code",
        "const base = 1;",
        "// Human comment",
        "// AI generated function",
        "function aiHelper() {",
        "  return true;",
        "}",
        "// Another human comment"
    ]);

    let merge_commit = repo
        .stage_all_and_commit("Merge feature via squash")
        .unwrap();
    let merge_sha = merge_commit.commit_sha;

    // Get the GitAi repository instance
    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    // Call the CI rewrite function
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;
    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feature",
        "main",
        &feature_sha,
        &merge_sha,
        false,
    )
    .unwrap();

    // Verify mixed authorship is preserved
    file.assert_lines_and_blame(crate::lines![
        "// Base code".human(),
        "const base = 1;".human(),
        "// Human comment".ai(),
        "// AI generated function".ai(),
        "function aiHelper() {".ai(),
        "  return true;".ai(),
        "}".ai(),
        "// Another human comment".human()
    ]);
}

/// Test squash merge where source commits have notes but no AI attestations.
#[test]
fn test_ci_squash_merge_empty_notes_preserved() {
    let repo = direct_test_repo();
    let mut file = repo.filename("feature.txt");

    file.set_contents(crate::lines!["base"]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.set_contents(crate::lines!["base", "human change"]);
    let feature_commit = repo.stage_all_and_commit("Human change").unwrap();
    let feature_sha = feature_commit.commit_sha;

    repo.git(&["checkout", "main"]).unwrap();
    file.set_contents(crate::lines!["base", "human change"]);
    let merge_commit = repo
        .stage_all_and_commit("Merge feature via squash")
        .unwrap();
    let merge_sha = merge_commit.commit_sha;

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;
    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feature",
        "main",
        &feature_sha,
        &merge_sha,
        false,
    )
    .unwrap();

    let authorship_log = get_reference_as_authorship_log_v3(&git_ai_repo, &merge_sha).unwrap();
    assert!(
        authorship_log.attestations.is_empty(),
        "Expected empty attestations for human-only squash merge"
    );
}

/// Test squash merge where source commits have no notes at all.
#[test]
fn test_ci_squash_merge_no_notes_no_authorship_created() {
    let repo = direct_test_repo();

    repo.git_og(&["config", "user.name", "Test User"]).unwrap();
    repo.git_og(&["config", "user.email", "test@example.com"])
        .unwrap();

    let mut file = repo.filename("feature.txt");
    file.set_contents(crate::lines!["base"]);
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "Initial commit"]).unwrap();
    repo.git_og(&["branch", "-M", "main"]).unwrap();

    repo.git_og(&["checkout", "-b", "feature"]).unwrap();
    file.set_contents(crate::lines!["base", "human change"]);
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "Human change"]).unwrap();
    let feature_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    repo.git_og(&["checkout", "main"]).unwrap();
    file.set_contents(crate::lines!["base", "human change"]);
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "Merge feature via squash"])
        .unwrap();
    let merge_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;
    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feature",
        "main",
        &feature_sha,
        &merge_sha,
        false,
    )
    .unwrap();

    assert!(
        get_reference_as_authorship_log_v3(&git_ai_repo, &merge_sha).is_err(),
        "Expected no authorship log when source commits have no notes"
    );
}

/// Test squash merge where conflict resolution adds content
#[test]
fn test_ci_squash_merge_with_manual_changes() {
    let repo = direct_test_repo();
    let mut file = repo.filename("config.js");

    // Create initial commit
    file.set_contents(crate::lines!["const config = {", "  version: 1", "};"]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with AI additions
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    file.set_contents(crate::lines![
        "const config = {",
        "  version: 1,",
        "  // AI added feature flag".ai(),
        "  enableAI: true".ai(),
        "};"
    ]);

    let feature_commit = repo.stage_all_and_commit("Add AI config").unwrap();
    let feature_sha = feature_commit.commit_sha;

    // Simulate CI squash merge with manual adjustment during merge
    // (e.g., developer manually tweaks formatting or adds extra config)
    repo.git(&["checkout", "main"]).unwrap();

    file.set_contents(crate::lines![
        "const config = {",
        "  version: 1,",
        "  // AI added feature flag",
        "  enableAI: true,",
        "  // Manual addition during merge",
        "  production: false",
        "};"
    ]);

    let merge_commit = repo
        .stage_all_and_commit("Merge feature via squash with tweaks")
        .unwrap();
    let merge_sha = merge_commit.commit_sha;

    // Get the GitAi repository instance
    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    // Call the CI rewrite function
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;
    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feature",
        "main",
        &feature_sha,
        &merge_sha,
        false,
    )
    .unwrap();

    // Verify AI authorship is preserved for AI lines, human for manual additions
    file.assert_lines_and_blame(crate::lines![
        "const config = {".human(),
        "  version: 1,".human(),
        "  // AI added feature flag".ai(),
        "  enableAI: true,".ai(),
        "  // Manual addition during merge".human(),
        "  production: false".human(),
        "};".human()
    ]);
}

/// Test rebase-like merge (multiple commits squashed) with AI content
#[test]
fn test_ci_rebase_merge_multiple_commits() {
    let repo = direct_test_repo();
    let mut file = repo.filename("app.js");

    // Create initial commit
    file.set_contents(crate::lines!["// App v1", ""]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with multiple commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First commit: AI adds function
    file.insert_at(
        1,
        crate::lines!["// AI function 1".ai(), "function ai1() { }".ai()],
    );
    repo.stage_all_and_commit("Add AI function 1").unwrap();

    // Second commit: AI adds another function
    file.insert_at(
        3,
        crate::lines!["// AI function 2".ai(), "function ai2() { }".ai()],
    );
    repo.stage_all_and_commit("Add AI function 2").unwrap();

    // Third commit: Human adds function
    file.insert_at(
        5,
        crate::lines!["// Human function", "function human() { }"],
    );
    let feature_commit = repo.stage_all_and_commit("Add human function").unwrap();
    let feature_sha = feature_commit.commit_sha;

    // Simulate CI rebase-style merge (all commits squashed into one)
    repo.git(&["checkout", "main"]).unwrap();

    file.set_contents(crate::lines![
        "// App v1",
        "// AI function 1",
        "function ai1() { }",
        "// AI function 2",
        "function ai2() { }",
        "// Human function",
        "function human() { }"
    ]);

    let merge_commit = repo
        .stage_all_and_commit("Merge feature branch (squashed)")
        .unwrap();
    let merge_sha = merge_commit.commit_sha;

    // Get the GitAi repository instance
    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    // Call the CI rewrite function
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;
    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feature",
        "main",
        &feature_sha,
        &merge_sha,
        false,
    )
    .unwrap();

    // Verify all authorship is correctly attributed
    file.assert_lines_and_blame(crate::lines![
        "// App v1".human(),
        "// AI function 1".ai(),
        "function ai1() { }".ai(),
        "// AI function 2".ai(),
        "function ai2() { }".ai(),
        "// Human function".human(),
        "function human() { }".human()
    ]);
}

/// Test that CI rebase merge correctly pairs original commits with rebased commits
/// in oldest-first order, so that each rebased commit's authorship note references
/// only the files from its corresponding original commit.
///
/// This is a regression test for a bug where `CommitRange::all_commits()` returned
/// commits in newest-first order (from `git rev-list`), but
/// `rewrite_authorship_after_rebase_v2` expects oldest-first. Without the
/// `.reverse()` fix in `ci_context.rs`, the positional pairing in
/// `pair_commits_for_rewrite` would be inverted: the first original commit's note
/// would be written to the last rebased commit and vice versa.
#[test]
fn test_ci_rebase_merge_commit_order_pairing() {
    use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
    use git_ai::ci::ci_context::{CiContext, CiEvent, CiRunOptions};

    let repo = direct_test_repo();

    // --- Set up initial commit on main ---
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    let base_sha = repo
        .stage_all_and_commit("Initial commit")
        .unwrap()
        .commit_sha;
    repo.git(&["branch", "-M", "main"]).unwrap();

    // --- Create feature branch with 2 commits, each touching a DIFFERENT file ---
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Commit 1 (older): AI adds file_a.txt
    let mut file_a = repo.filename("file_a.txt");
    file_a.set_contents(crate::lines!["ai content in file_a".ai()]);
    let feature_sha1 = repo.stage_all_and_commit("Add file_a").unwrap().commit_sha;

    // Commit 2 (newer): AI adds file_b.txt
    let mut file_b = repo.filename("file_b.txt");
    file_b.set_contents(crate::lines!["ai content in file_b".ai()]);
    let feature_sha2 = repo.stage_all_and_commit("Add file_b").unwrap().commit_sha;

    // --- Simulate rebase merge on main ---
    // A rebase merge produces N new linear commits on main (not a single squash commit).
    // We simulate this by cherry-picking each feature commit onto main.
    repo.git(&["checkout", "main"]).unwrap();

    repo.git_og(&["cherry-pick", &feature_sha1]).unwrap();
    let new_sha1 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.git_og(&["cherry-pick", &feature_sha2]).unwrap();
    let new_sha2 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // --- Set up a bare origin so CiContext.push_authorship() can succeed ---
    let origin_dir = tempfile::tempdir().unwrap();
    let origin_path = origin_dir.path().join("origin.git");
    repo.git_og(&[
        "clone",
        "--bare",
        repo.path().to_str().unwrap(),
        origin_path.to_str().unwrap(),
    ])
    .unwrap();
    repo.git_og(&["remote", "add", "origin", origin_path.to_str().unwrap()])
        .unwrap();

    // --- Run CiContext ---
    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    let event = CiEvent::Merge {
        merge_commit_sha: new_sha2.clone(),
        head_ref: "feature".to_string(),
        head_sha: feature_sha2.clone(),
        base_ref: "main".to_string(),
        base_sha,
    };

    let ctx = CiContext::with_repository(git_ai_repo, event);
    let result = ctx.run_with_options(CiRunOptions {
        skip_fetch_notes: true,
        skip_fetch_base: true,
    });
    assert!(
        result.is_ok(),
        "CiContext run should succeed, got: {:?}",
        result
    );

    // --- Verify: each rebased commit's note references the correct file ---
    // If the order bug were present (newest-first instead of oldest-first),
    // new_sha1 would get file_b's note and new_sha2 would get file_a's note.

    let note1 = repo
        .read_authorship_note(&new_sha1)
        .expect("rebased commit 1 should have authorship note");
    let note2 = repo
        .read_authorship_note(&new_sha2)
        .expect("rebased commit 2 should have authorship note");

    let files1: Vec<String> = AuthorshipLog::deserialize_from_string(&note1)
        .unwrap()
        .attestations
        .iter()
        .map(|a| a.file_path.clone())
        .collect();
    let files2: Vec<String> = AuthorshipLog::deserialize_from_string(&note2)
        .unwrap()
        .attestations
        .iter()
        .map(|a| a.file_path.clone())
        .collect();

    // Rebased commit 1 (older) should have file_a.txt (NOT file_b.txt)
    assert!(
        files1.iter().any(|f| f.contains("file_a")),
        "Rebased commit 1's note should reference file_a.txt, but found: {:?}",
        files1
    );
    assert!(
        !files1.iter().any(|f| f.contains("file_b")),
        "COMMIT ORDER BUG: Rebased commit 1's note references file_b.txt \
         (from the LAST original commit). This means original_commits was \
         newest-first instead of oldest-first. Found: {:?}",
        files1
    );

    // Rebased commit 2 (newer) should have file_b.txt (NOT file_a.txt)
    assert!(
        files2.iter().any(|f| f.contains("file_b")),
        "Rebased commit 2's note should reference file_b.txt, but found: {:?}",
        files2
    );
    assert!(
        !files2.iter().any(|f| f.contains("file_a")),
        "COMMIT ORDER BUG: Rebased commit 2's note references file_a.txt \
         (from the FIRST original commit). This means original_commits was \
         newest-first instead of oldest-first. Found: {:?}",
        files2
    );
}

crate::reuse_tests_in_worktree!(
    test_ci_squash_merge_basic,
    test_ci_squash_merge_multiple_files,
    test_ci_squash_merge_mixed_content,
    test_ci_squash_merge_empty_notes_preserved,
    test_ci_squash_merge_no_notes_no_authorship_created,
    test_ci_squash_merge_with_manual_changes,
    test_ci_rebase_merge_multiple_commits,
    test_ci_rebase_merge_commit_order_pairing,
);
