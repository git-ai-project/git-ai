#[macro_use]
mod repos;
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
    file.set_contents(lines!["// App v1"]);
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

#[test]
fn test_squash_merge_with_duplicate_lines() {
    // This test verifies that squash merge correctly preserves AI authorship for duplicate lines
    let repo = TestRepo::new();
    let mut file = repo.filename("helpers.rs");

    // Create master branch with first function (human-authored)
    file.set_contents(lines![
        "pub fn format_string(s: &str) -> String {",
        "    s.to_uppercase()",
        "}",
    ]);
    repo.stage_all_and_commit("Add format_string function")
        .unwrap();

    // Save the default branch name
    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // AI adds a second function
    // The key test: the second `}` on line 6 is AI-authored, but there's already a `}` on line 3
    file.set_contents(lines![
        "pub fn format_string(s: &str) -> String {",
        "    s.to_uppercase()",
        "}",
        "pub fn reverse_string(s: &str) -> String {".ai(),
        "    s.chars().rev().collect()".ai(),
        "}".ai(), // Duplicate closing brace authored by ai
    ]);

    let output = repo
        .stage_all_and_commit("AI adds reverse_string function")
        .unwrap();

    // Switch back to default branch
    println!(
        "checkout: {:?}",
        repo.git(&["checkout", &default_branch]).unwrap()
    );

    // Squash merge feature branch
    let output = repo.git(&["merge", "--squash", "feature"]).unwrap();
    println!("output: {:?}", output);

    let working_log = repo.current_working_logs();
    println!(
        "initial attributions: {:?}",
        working_log.read_initial_attributions()
    );

    println!(
        "stage and commit: {:?}",
        repo.stage_all_and_commit("Squash merge: add reverse_string")
            .unwrap()
    );

    println!(
        "diff: {:?}",
        repo.git(&["diff", &default_branch, "feature", "--", "helpers.rs"])
            .unwrap()
    );

    // This should pass but currently fails
    file = repo.filename("helpers.rs");
    file.assert_lines_and_blame(lines![
        "pub fn format_string(s: &str) -> String {".human(),
        "    s.to_uppercase()".human(),
        "}".human(),
        "pub fn reverse_string(s: &str) -> String {".ai(),
        "    s.chars().rev().collect()".ai(),
        "}".ai(), // Should be AI
    ]);
}
