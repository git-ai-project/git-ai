#[macro_use]
mod repos;
use git_ai::git::refs::get_reference_as_authorship_log_v3;
use git_ai::git::repository as GitAiRepository;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

/// Test basic squash merge via CI - AI code from feature branch squashed into main
#[test]
fn test_ci_squash_merge_basic() {
    let repo = TestRepo::new();
    let mut file = repo.filename("feature.js");

    // Create initial commit on main (rename default branch to main)
    file.set_contents(lines!["// Original code", "function original() {}"]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with AI code
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(
        2,
        lines![
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
    file.set_contents(lines![
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
    file.assert_lines_and_blame(lines![
        "// Original code".human(),
        "function original() {}".human(),
        "// AI added function".ai(),
        "function aiFeature() {".ai(),
        "  return 'ai code';".ai(),
        "}".ai()
    ]);
}

/// Test squash merge with multiple files containing AI code
#[test]
fn test_ci_squash_merge_multiple_files() {
    let repo = TestRepo::new();

    // Create initial commit on main with two files
    let mut file1 = repo.filename("file1.js");
    let mut file2 = repo.filename("file2.js");

    file1.set_contents(lines!["// File 1 original"]);
    file2.set_contents(lines!["// File 2 original"]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with AI changes to both files
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    file1.insert_at(
        1,
        lines!["// AI code in file1".ai(), "const feature1 = 'ai';".ai()],
    );
    file2.insert_at(
        1,
        lines!["// AI code in file2".ai(), "const feature2 = 'ai';".ai()],
    );

    let feature_commit = repo
        .stage_all_and_commit("Add AI features to both files")
        .unwrap();
    let feature_sha = feature_commit.commit_sha;

    // Simulate CI squash merge
    repo.git(&["checkout", "main"]).unwrap();

    file1.set_contents(lines![
        "// File 1 original",
        "// AI code in file1",
        "const feature1 = 'ai';"
    ]);
    file2.set_contents(lines![
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
    file1.assert_lines_and_blame(lines![
        "// File 1 original".human(),
        "// AI code in file1".ai(),
        "const feature1 = 'ai';".ai()
    ]);

    file2.assert_lines_and_blame(lines![
        "// File 2 original".human(),
        "// AI code in file2".ai(),
        "const feature2 = 'ai';".ai()
    ]);
}

/// Test squash merge with mixed AI and human content
#[test]
fn test_ci_squash_merge_mixed_content() {
    let repo = TestRepo::new();
    let mut file = repo.filename("mixed.js");

    // Create initial commit
    file.set_contents(lines!["// Base code", "const base = 1;"]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with mixed AI and human changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Simulate: human adds a comment, AI adds code, human adds more
    file.insert_at(
        2,
        lines![
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

    file.set_contents(lines![
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
    file.assert_lines_and_blame(lines![
        "// Base code".human(),
        "const base = 1;".human(),
        "// Human comment".human(),
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
    let repo = TestRepo::new();
    let mut file = repo.filename("feature.txt");

    file.set_contents(lines!["base"]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.set_contents(lines!["base", "human change"]);
    let feature_commit = repo.stage_all_and_commit("Human change").unwrap();
    let feature_sha = feature_commit.commit_sha;

    repo.git(&["checkout", "main"]).unwrap();
    file.set_contents(lines!["base", "human change"]);
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
    let repo = TestRepo::new();

    repo.git_og(&["config", "user.name", "Test User"]).unwrap();
    repo.git_og(&["config", "user.email", "test@example.com"])
        .unwrap();

    let mut file = repo.filename("feature.txt");
    file.set_contents(lines!["base"]);
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "Initial commit"]).unwrap();
    repo.git_og(&["branch", "-M", "main"]).unwrap();

    repo.git_og(&["checkout", "-b", "feature"]).unwrap();
    file.set_contents(lines!["base", "human change"]);
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "Human change"]).unwrap();
    let feature_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    repo.git_og(&["checkout", "main"]).unwrap();
    file.set_contents(lines!["base", "human change"]);
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
    let repo = TestRepo::new();
    let mut file = repo.filename("config.js");

    // Create initial commit
    file.set_contents(lines!["const config = {", "  version: 1", "};"]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with AI additions
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    file.set_contents(lines![
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

    file.set_contents(lines![
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
    file.assert_lines_and_blame(lines![
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
    let repo = TestRepo::new();
    let mut file = repo.filename("app.js");

    // Create initial commit
    file.set_contents(lines!["// App v1", ""]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with multiple commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First commit: AI adds function
    file.insert_at(
        1,
        lines!["// AI function 1".ai(), "function ai1() { }".ai()],
    );
    repo.stage_all_and_commit("Add AI function 1").unwrap();

    // Second commit: AI adds another function
    file.insert_at(
        3,
        lines!["// AI function 2".ai(), "function ai2() { }".ai()],
    );
    repo.stage_all_and_commit("Add AI function 2").unwrap();

    // Third commit: Human adds function
    file.insert_at(5, lines!["// Human function", "function human() { }"]);
    let feature_commit = repo.stage_all_and_commit("Add human function").unwrap();
    let feature_sha = feature_commit.commit_sha;

    // Simulate CI rebase-style merge (all commits squashed into one)
    repo.git(&["checkout", "main"]).unwrap();

    file.set_contents(lines![
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
    file.assert_lines_and_blame(lines![
        "// App v1".human(),
        "// AI function 1".ai(),
        "function ai1() { }".ai(),
        "// AI function 2".ai(),
        "function ai2() { }".ai(),
        "// Human function".human(),
        "function human() { }".human()
    ]);
}

reuse_tests_in_worktree!(
    test_ci_squash_merge_basic,
    test_ci_squash_merge_multiple_files,
    test_ci_squash_merge_mixed_content,
    test_ci_squash_merge_empty_notes_preserved,
    test_ci_squash_merge_no_notes_no_authorship_created,
    test_ci_squash_merge_with_manual_changes,
    test_ci_rebase_merge_multiple_commits,
);
