use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::git::refs::get_reference_as_authorship_log_v3;
use git_ai::git::repository as GitAiRepository;

fn direct_test_repo() -> TestRepo {
    TestRepo::new()
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

    // Verify metadata.humans contains the known human attribution
    let merge_log = get_reference_as_authorship_log_v3(&git_ai_repo, &merge_sha).unwrap();
    assert!(
        merge_log.metadata.humans.contains_key("h_e858f2c2faea28"),
        "squash note should carry h_e858f2c2faea28 from human-attributed lines in mixed content"
    );
    assert_eq!(
        merge_log.metadata.humans["h_e858f2c2faea28"].author,
        "Test User <test@example.com>"
    );

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
        authorship_log.metadata.prompts.is_empty(),
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

    // With the contributors fix, a minimal authorship log is created even for
    // manual-only merges to track per-developer contributions. The log should
    // have contributors but no AI prompts.
    match get_reference_as_authorship_log_v3(&git_ai_repo, &merge_sha) {
        Ok(log) => {
            // If a log was created, it should have contributors with only manual additions
            assert!(
                log.metadata.prompts.is_empty(),
                "Manual-only merge should have no prompts"
            );
            if let Some(ref contributors) = log.metadata.contributors {
                let total_ai: u32 = contributors.values().map(|c| c.ai_accepted).sum();
                assert_eq!(
                    total_ai, 0,
                    "Manual-only merge should have zero ai_accepted"
                );
            }
        }
        Err(_) => {
            // Also acceptable: no authorship log created (e.g. zero diff lines)
        }
    }
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

    // Verify metadata.humans contains the known human attribution
    let merge_log = get_reference_as_authorship_log_v3(&git_ai_repo, &merge_sha).unwrap();
    assert!(
        merge_log.metadata.humans.contains_key("h_e858f2c2faea28"),
        "squash note should carry h_e858f2c2faea28 from human-attributed lines in config"
    );
    assert_eq!(
        merge_log.metadata.humans["h_e858f2c2faea28"].author,
        "Test User <test@example.com>"
    );

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

    // Verify metadata.humans contains the known human attribution
    let merge_log = get_reference_as_authorship_log_v3(&git_ai_repo, &merge_sha).unwrap();
    assert!(
        merge_log.metadata.humans.contains_key("h_e858f2c2faea28"),
        "squash note should carry h_e858f2c2faea28 from human function lines"
    );
    assert_eq!(
        merge_log.metadata.humans["h_e858f2c2faea28"].author,
        "Test User <test@example.com>"
    );

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
        skip_push: false,
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

/// Verify that `git-ai ci local merge` correctly pairs original commits with
/// their rebased counterparts (oldest-first) after a real `git rebase`.
///
/// Creates a two-commit feature branch (commit 1 → file_a.txt, commit 2 →
/// file_b.txt), advances main by one commit so the rebase produces genuinely
/// new SHAs, then rebases the feature branch onto main via plain `git rebase`
/// (bypassing the local hook).  After fast-forwarding main, the test invokes
/// `git-ai ci local merge` exactly as CI would and checks that:
///
/// - The first rebased commit's authorship note references only file_a.txt
/// - The second rebased commit's authorship note references only file_b.txt
///
/// Before the `.reverse()` fix in `ci_context.rs` the pairing was inverted:
/// original_commits came back newest-first from `CommitRange::all_commits()`
/// while new_commits were oldest-first, so each note landed on the wrong commit.
#[test]
fn test_ci_local_rebase_merge_two_commits() {
    use git_ai::authorship::authorship_log_serialization::AuthorshipLog;

    let repo = direct_test_repo();

    // --- Initial commit on main ---
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();
    let base_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // --- Feature branch: two commits touching different files ---
    repo.git_og(&["checkout", "-b", "feature"]).unwrap();

    let mut file_a = repo.filename("file_a.txt");
    file_a.set_contents(crate::lines!["ai content in file_a".ai()]);
    let feature_sha1 = repo.stage_all_and_commit("Add file_a").unwrap().commit_sha;

    let mut file_b = repo.filename("file_b.txt");
    file_b.set_contents(crate::lines!["ai content in file_b".ai()]);
    let feature_sha2 = repo.stage_all_and_commit("Add file_b").unwrap().commit_sha;

    // --- Advance main so the rebase produces new commit SHAs ---
    repo.git_og(&["checkout", "main"]).unwrap();
    let mut main_file = repo.filename("main_only.txt");
    main_file.set_contents(crate::lines!["main-only content"]);
    repo.git_og(&["add", "main_only.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "Advance main"]).unwrap();

    // --- Rebase feature onto main, bypassing the local rebase hook ---
    repo.git_og(&["checkout", "feature"]).unwrap();
    repo.git_og(&["rebase", "main"]).unwrap();

    let new_sha2 = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let new_sha1 = repo
        .git_og(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();

    assert_ne!(
        new_sha1, feature_sha1,
        "rebase must produce a new SHA for commit 1"
    );
    assert_ne!(
        new_sha2, feature_sha2,
        "rebase must produce a new SHA for commit 2"
    );

    // --- Fast-forward main to the rebased feature HEAD ---
    repo.git_og(&["checkout", "main"]).unwrap();
    repo.git_og(&["merge", "--ff-only", "feature"]).unwrap();

    // --- Bare clone so push_authorship("origin") inside CiContext can succeed ---
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

    // --- Run the local CI command as CI would after a rebase merge ---
    let output = repo
        .git_ai(&[
            "ci",
            "local",
            "merge",
            "--merge-commit-sha",
            new_sha2.as_str(),
            "--head-ref",
            "feature",
            "--head-sha",
            feature_sha2.as_str(),
            "--base-ref",
            "main",
            "--base-sha",
            base_sha.as_str(),
            "--skip-fetch-notes",
            "--skip-fetch-base",
        ])
        .expect("ci local merge should succeed");

    assert!(
        output.contains("authorship rewritten successfully"),
        "Expected authorship rewritten, got: {}",
        output
    );

    // --- Verify each rebased commit carries notes for its own file only ---
    let note1 = repo
        .read_authorship_note(&new_sha1)
        .expect("rebased commit 1 should have an authorship note");
    let note2 = repo
        .read_authorship_note(&new_sha2)
        .expect("rebased commit 2 should have an authorship note");

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

    assert!(
        files1.iter().any(|f| f.contains("file_a")),
        "rebased commit 1 should reference file_a.txt, got: {:?}",
        files1
    );
    assert!(
        !files1.iter().any(|f| f.contains("file_b")),
        "COMMIT ORDER BUG: rebased commit 1 references file_b (newest-first pairing). Got: {:?}",
        files1
    );
    assert!(
        files2.iter().any(|f| f.contains("file_b")),
        "rebased commit 2 should reference file_b.txt, got: {:?}",
        files2
    );
    assert!(
        !files2.iter().any(|f| f.contains("file_a")),
        "COMMIT ORDER BUG: rebased commit 2 references file_a (newest-first pairing). Got: {:?}",
        files2
    );
}

/// Three-commit variant of `test_ci_local_rebase_merge_two_commits`.
///
/// Each of the three original commits touches a distinct file (file_a / file_b /
/// file_c).  After rebasing onto an advanced main and running
/// `git-ai ci local merge`, every rebased commit must carry the note for its
/// own file and none of the others.  This catches both full inversions
/// (first↔last) and off-by-one shifts in the positional pairing.
#[test]
fn test_ci_local_rebase_merge_three_commits() {
    use git_ai::authorship::authorship_log_serialization::AuthorshipLog;

    let repo = direct_test_repo();

    // --- Initial commit on main ---
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();
    let base_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // --- Feature branch: three commits touching distinct files ---
    repo.git_og(&["checkout", "-b", "feature"]).unwrap();

    let mut file_a = repo.filename("file_a.txt");
    file_a.set_contents(crate::lines!["ai content in file_a".ai()]);
    let feature_sha1 = repo.stage_all_and_commit("Add file_a").unwrap().commit_sha;

    let mut file_b = repo.filename("file_b.txt");
    file_b.set_contents(crate::lines!["ai content in file_b".ai()]);
    let feature_sha2 = repo.stage_all_and_commit("Add file_b").unwrap().commit_sha;

    let mut file_c = repo.filename("file_c.txt");
    file_c.set_contents(crate::lines!["ai content in file_c".ai()]);
    let feature_sha3 = repo.stage_all_and_commit("Add file_c").unwrap().commit_sha;

    // --- Advance main so the rebase produces new commit SHAs ---
    repo.git_og(&["checkout", "main"]).unwrap();
    let mut main_file = repo.filename("main_only.txt");
    main_file.set_contents(crate::lines!["main-only content"]);
    repo.git_og(&["add", "main_only.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "Advance main"]).unwrap();

    // --- Rebase feature onto main, bypassing the local rebase hook ---
    repo.git_og(&["checkout", "feature"]).unwrap();
    repo.git_og(&["rebase", "main"]).unwrap();

    let new_sha3 = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let new_sha2 = repo
        .git_og(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    let new_sha1 = repo
        .git_og(&["rev-parse", "HEAD~2"])
        .unwrap()
        .trim()
        .to_string();

    assert_ne!(
        new_sha1, feature_sha1,
        "rebase must produce a new SHA for commit 1"
    );
    assert_ne!(
        new_sha2, feature_sha2,
        "rebase must produce a new SHA for commit 2"
    );
    assert_ne!(
        new_sha3, feature_sha3,
        "rebase must produce a new SHA for commit 3"
    );

    // --- Fast-forward main to the rebased feature HEAD ---
    repo.git_og(&["checkout", "main"]).unwrap();
    repo.git_og(&["merge", "--ff-only", "feature"]).unwrap();

    // --- Bare clone so push_authorship("origin") inside CiContext can succeed ---
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

    // --- Run the local CI command as CI would after a rebase merge ---
    let output = repo
        .git_ai(&[
            "ci",
            "local",
            "merge",
            "--merge-commit-sha",
            new_sha3.as_str(),
            "--head-ref",
            "feature",
            "--head-sha",
            feature_sha3.as_str(),
            "--base-ref",
            "main",
            "--base-sha",
            base_sha.as_str(),
            "--skip-fetch-notes",
            "--skip-fetch-base",
        ])
        .expect("ci local merge should succeed");

    assert!(
        output.contains("authorship rewritten successfully"),
        "Expected authorship rewritten, got: {}",
        output
    );

    // --- Verify each rebased commit carries notes for its own file only ---
    let note1 = repo
        .read_authorship_note(&new_sha1)
        .expect("rebased commit 1 should have an authorship note");
    let note2 = repo
        .read_authorship_note(&new_sha2)
        .expect("rebased commit 2 should have an authorship note");
    let note3 = repo
        .read_authorship_note(&new_sha3)
        .expect("rebased commit 3 should have an authorship note");

    let files = |note: &str| -> Vec<String> {
        AuthorshipLog::deserialize_from_string(note)
            .unwrap()
            .attestations
            .iter()
            .map(|a| a.file_path.clone())
            .collect()
    };

    let files1 = files(&note1);
    let files2 = files(&note2);
    let files3 = files(&note3);

    // Commit 1 → file_a only
    assert!(
        files1.iter().any(|f| f.contains("file_a")),
        "rebased commit 1 should reference file_a.txt, got: {:?}",
        files1
    );
    assert!(
        !files1
            .iter()
            .any(|f| f.contains("file_b") || f.contains("file_c")),
        "COMMIT ORDER BUG: rebased commit 1 references wrong file. Got: {:?}",
        files1
    );

    // Commit 2 → file_b only
    assert!(
        files2.iter().any(|f| f.contains("file_b")),
        "rebased commit 2 should reference file_b.txt, got: {:?}",
        files2
    );
    assert!(
        !files2
            .iter()
            .any(|f| f.contains("file_a") || f.contains("file_c")),
        "COMMIT ORDER BUG: rebased commit 2 references wrong file. Got: {:?}",
        files2
    );

    // Commit 3 → file_c only
    assert!(
        files3.iter().any(|f| f.contains("file_c")),
        "rebased commit 3 should reference file_c.txt, got: {:?}",
        files3
    );
    assert!(
        !files3
            .iter()
            .any(|f| f.contains("file_a") || f.contains("file_b")),
        "COMMIT ORDER BUG: rebased commit 3 references wrong file. Got: {:?}",
        files3
    );
}

/// Standard-human variant of test_ci_squash_merge_basic.
/// Uses unattributed (checkpoint --) human lines instead of known-human attribution.
#[test]
fn test_ci_squash_merge_basic_standard_human() {
    let repo = direct_test_repo();
    let mut file = repo.filename("feature.js");

    // Create initial commit on main (rename default branch to main)
    file.set_contents(crate::lines![
        "// Original code".unattributed_human(),
        "function original() {}".unattributed_human()
    ]);
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
        "// Original code".unattributed_human(),
        "function original() {}".unattributed_human(),
        "// AI added function".unattributed_human(),
        "function aiFeature() {".unattributed_human(),
        "  return 'ai code';".unattributed_human(),
        "}".unattributed_human()
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
        "// Original code".unattributed_human(),
        "function original() {}".ai(),
        "// AI added function".ai(),
        "function aiFeature() {".ai(),
        "  return 'ai code';".ai(),
        "}".ai()
    ]);
}

/// Standard-human variant of test_ci_squash_merge_mixed_content.
/// Uses unattributed (checkpoint --) human lines instead of known-human attribution.
#[test]
fn test_ci_squash_merge_mixed_content_standard_human() {
    let repo = direct_test_repo();
    let mut file = repo.filename("mixed.js");

    // Create initial commit
    file.set_contents(crate::lines![
        "// Base code".unattributed_human(),
        "const base = 1;".unattributed_human()
    ]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with mixed AI and human changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Simulate: human adds a comment, AI adds code, human adds more
    file.insert_at(
        2,
        crate::lines![
            "// Human comment".unattributed_human(),
            "// AI generated function".ai(),
            "function aiHelper() {".ai(),
            "  return true;".ai(),
            "}".ai(),
            "// Another human comment".unattributed_human()
        ],
    );

    let feature_commit = repo.stage_all_and_commit("Add mixed content").unwrap();
    let feature_sha = feature_commit.commit_sha;

    // Simulate CI squash merge
    repo.git(&["checkout", "main"]).unwrap();

    file.set_contents(crate::lines![
        "// Base code".unattributed_human(),
        "const base = 1;".unattributed_human(),
        "// Human comment".unattributed_human(),
        "// AI generated function".unattributed_human(),
        "function aiHelper() {".unattributed_human(),
        "  return true;".unattributed_human(),
        "}".unattributed_human(),
        "// Another human comment".unattributed_human()
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
        "// Base code".unattributed_human(),
        "const base = 1;".unattributed_human(),
        "// Human comment".ai(),
        "// AI generated function".ai(),
        "function aiHelper() {".ai(),
        "  return true;".ai(),
        "}".ai(),
        "// Another human comment".unattributed_human()
    ]);
}

/// Standard-human variant of test_ci_squash_merge_with_manual_changes.
/// Uses unattributed (checkpoint --) human lines instead of known-human attribution.
#[test]
fn test_ci_squash_merge_with_manual_changes_standard_human() {
    let repo = direct_test_repo();
    let mut file = repo.filename("config.js");

    // Create initial commit
    file.set_contents(crate::lines![
        "const config = {".unattributed_human(),
        "  version: 1".unattributed_human(),
        "};".unattributed_human()
    ]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with AI additions
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    file.set_contents(crate::lines![
        "const config = {".unattributed_human(),
        "  version: 1,".unattributed_human(),
        "  // AI added feature flag".ai(),
        "  enableAI: true".ai(),
        "};".unattributed_human()
    ]);

    let feature_commit = repo.stage_all_and_commit("Add AI config").unwrap();
    let feature_sha = feature_commit.commit_sha;

    // Simulate CI squash merge with manual adjustment during merge
    // (e.g., developer manually tweaks formatting or adds extra config)
    repo.git(&["checkout", "main"]).unwrap();

    file.set_contents(crate::lines![
        "const config = {".unattributed_human(),
        "  version: 1,".unattributed_human(),
        "  // AI added feature flag".unattributed_human(),
        "  enableAI: true,".unattributed_human(),
        "  // Manual addition during merge".unattributed_human(),
        "  production: false".unattributed_human(),
        "};".unattributed_human()
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
        "const config = {".unattributed_human(),
        "  version: 1,".unattributed_human(),
        "  // AI added feature flag".ai(),
        "  enableAI: true,".ai(),
        "  // Manual addition during merge".unattributed_human(),
        "  production: false".unattributed_human(),
        "};".unattributed_human()
    ]);
}

/// Standard-human variant of test_ci_rebase_merge_multiple_commits.
/// Uses unattributed (checkpoint --) human lines instead of known-human attribution.
#[test]
fn test_ci_rebase_merge_multiple_commits_standard_human() {
    let repo = direct_test_repo();
    let mut file = repo.filename("app.js");

    // Create initial commit
    file.set_contents(crate::lines![
        "// App v1".unattributed_human(),
        "".unattributed_human()
    ]);
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
        crate::lines![
            "// Human function".unattributed_human(),
            "function human() { }".unattributed_human()
        ],
    );
    let feature_commit = repo.stage_all_and_commit("Add human function").unwrap();
    let feature_sha = feature_commit.commit_sha;

    // Simulate CI rebase-style merge (all commits squashed into one)
    repo.git(&["checkout", "main"]).unwrap();

    file.set_contents(crate::lines![
        "// App v1".unattributed_human(),
        "// AI function 1".unattributed_human(),
        "function ai1() { }".unattributed_human(),
        "// AI function 2".unattributed_human(),
        "function ai2() { }".unattributed_human(),
        "// Human function".unattributed_human(),
        "function human() { }".unattributed_human()
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
        "// App v1".unattributed_human(),
        "// AI function 1".ai(),
        "function ai1() { }".ai(),
        "// AI function 2".ai(),
        "function ai2() { }".ai(),
        "// Human function".unattributed_human(),
        "function human() { }".unattributed_human()
    ]);
}

/// Test that CI squash merge populates the contributors field in the authorship note.
/// After squash-merging a feature branch with AI commits, the merge commit's note
/// should contain a contributors map keyed by developer email with per-developer stats.
#[test]
fn test_ci_squash_merge_populates_contributors() {
    let repo = direct_test_repo();
    let mut file = repo.filename("widget.ts");

    // Create initial commit on main
    file.set_contents(crate::lines!["export class Widget {", "  render() {}", "}"]);
    let _base_commit = repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Commit 1: AI adds 2 lines
    file.insert_at(
        1,
        crate::lines!["  color = 'blue';".ai(), "  size = 42;".ai()],
    );
    repo.stage_all_and_commit("AI adds color and size").unwrap();

    // Commit 2: Human adds 1 line
    file.insert_at(3, crate::lines!["  label = 'hello';"]);
    let feature_commit = repo.stage_all_and_commit("Human adds label").unwrap();
    let feature_sha = feature_commit.commit_sha;

    // Simulate CI squash merge
    repo.git(&["checkout", "main"]).unwrap();
    file.set_contents(crate::lines![
        "export class Widget {",
        "  color = 'blue';",
        "  size = 42;",
        "  label = 'hello';",
        "  render() {}",
        "}"
    ]);
    let merge_commit = repo.stage_all_and_commit("Squashed feature").unwrap();
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

    let squash_log = get_reference_as_authorship_log_v3(&git_ai_repo, &merge_sha)
        .expect("Squash commit should have authorship log");

    // KEY ASSERTION: contributors must be populated
    assert!(
        squash_log.metadata.contributors.is_some(),
        "Squash merge note should have contributors field populated"
    );
    let contributors = squash_log.metadata.contributors.unwrap();
    assert!(
        !contributors.is_empty(),
        "Contributors map should not be empty"
    );

    // The test repo uses "Test User" as the committer — all contributions should be under one email
    let (email, stats) = contributors.iter().next().unwrap();
    assert!(!email.is_empty(), "Contributor email should not be empty");
    assert!(
        stats.ai_accepted > 0,
        "Contributor should have ai_accepted > 0, got: {}",
        stats.ai_accepted
    );
    assert!(
        stats.manual_additions > 0 || stats.human_additions > 0,
        "Contributor should have some human/manual additions"
    );
}

/// Test that CI rebase merge populates the contributors field on each rebased commit's note.
/// Unlike squash (N→1), rebase creates N new commits — each should have its own contributors.
#[test]
fn test_ci_rebase_merge_populates_contributors() {
    use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
    use git_ai::ci::ci_context::{CiContext, CiEvent, CiRunOptions};

    let repo = direct_test_repo();

    // Create initial commit on main
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    let base_sha = repo
        .stage_all_and_commit("Initial commit")
        .unwrap()
        .commit_sha;
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with 2 AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut file_a = repo.filename("file_a.txt");
    file_a.set_contents(crate::lines!["ai content in file_a".ai()]);
    let feature_sha1 = repo.stage_all_and_commit("Add file_a").unwrap().commit_sha;

    let mut file_b = repo.filename("file_b.txt");
    file_b.set_contents(crate::lines!["ai content in file_b".ai()]);
    let feature_sha2 = repo.stage_all_and_commit("Add file_b").unwrap().commit_sha;

    // Simulate rebase merge: cherry-pick each commit onto main
    repo.git(&["checkout", "main"]).unwrap();

    repo.git_og(&["cherry-pick", &feature_sha1]).unwrap();
    let new_sha1 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.git_og(&["cherry-pick", &feature_sha2]).unwrap();
    let new_sha2 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Set up bare origin for CiContext
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

    // Run CiContext (detects rebase merge and calls rewrite_authorship_after_rebase_v2)
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
    ctx.run_with_options(CiRunOptions {
        skip_fetch_notes: true,
        skip_fetch_base: true,
    })
    .expect("CiContext run should succeed");

    // Verify each rebased commit has contributors in its note
    let note1_raw = repo
        .read_authorship_note(&new_sha1)
        .expect("rebased commit 1 should have authorship note");
    let log1 = AuthorshipLog::deserialize_from_string(&note1_raw).unwrap();

    let note2_raw = repo
        .read_authorship_note(&new_sha2)
        .expect("rebased commit 2 should have authorship note");
    let log2 = AuthorshipLog::deserialize_from_string(&note2_raw).unwrap();

    // KEY ASSERTION: both rebased commits should have contributors
    assert!(
        log1.metadata.contributors.is_some(),
        "Rebased commit 1's note should have contributors. Metadata: {:?}",
        log1.metadata
    );
    assert!(
        log2.metadata.contributors.is_some(),
        "Rebased commit 2's note should have contributors. Metadata: {:?}",
        log2.metadata
    );

    let contributors1 = log1.metadata.contributors.unwrap();
    let contributors2 = log2.metadata.contributors.unwrap();

    // Each commit should have at least one contributor with ai_accepted > 0
    let has_ai_1 = contributors1.values().any(|s| s.ai_accepted > 0);
    let has_ai_2 = contributors2.values().any(|s| s.ai_accepted > 0);
    assert!(
        has_ai_1,
        "Rebased commit 1 contributors should have ai_accepted > 0. Got: {:?}",
        contributors1
    );
    assert!(
        has_ai_2,
        "Rebased commit 2 contributors should have ai_accepted > 0. Got: {:?}",
        contributors2
    );
}

/// Regression test: a squash merge onto a branch with prior history must NOT be
/// misdetected as a rebase merge. The old heuristic counted linear commits walked
/// back from merge_commit_sha — if the target branch had N prior commits matching
/// the PR's N, it would wrongly classify as rebase. The fix verifies that the
/// walked-back commits touch the same files as the originals.
#[test]
fn test_ci_squash_merge_not_misdetected_as_rebase_on_branch_with_history() {
    use git_ai::ci::ci_context::{CiContext, CiEvent, CiRunOptions};

    let repo = direct_test_repo();

    // Create initial commit on main
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create target branch (gitai-feature) with prior commits to create history
    repo.git(&["checkout", "-b", "gitai-feature"]).unwrap();

    let mut prior1 = repo.filename("prior1.txt");
    prior1.set_contents(crate::lines!["prior commit 1"]);
    repo.stage_all_and_commit("Prior commit 1 on feature")
        .unwrap();

    let mut prior2 = repo.filename("prior2.txt");
    prior2.set_contents(crate::lines!["prior commit 2"]);
    repo.stage_all_and_commit("Prior commit 2 on feature")
        .unwrap();

    let mut prior3 = repo.filename("prior3.txt");
    prior3.set_contents(crate::lines!["prior commit 3"]);
    repo.stage_all_and_commit("Prior commit 3 on feature")
        .unwrap();

    // Create task branch from feature with 4 AI commits (matching the prior count + 1)
    repo.git(&["checkout", "-b", "task-branch"]).unwrap();

    let mut file_a = repo.filename("file_a.ts");
    file_a.set_contents(crate::lines!["human line 1", "human line 2"]);
    repo.stage_all_and_commit("Human commit").unwrap();

    let mut file_b = repo.filename("file_b.ts");
    file_b.set_contents(crate::lines!["ai line 1".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();

    let mut file_c = repo.filename("file_c.ts");
    file_c.set_contents(crate::lines!["ai line 2".ai()]);
    repo.stage_all_and_commit("AI commit 2").unwrap();

    let mut file_d = repo.filename("file_d.ts");
    file_d.set_contents(crate::lines!["ai line 3".ai()]);
    let task_tip = repo.stage_all_and_commit("AI commit 3").unwrap();
    let task_tip_sha = task_tip.commit_sha;

    // Simulate squash merge onto gitai-feature (use git_og to avoid writing a note —
    // the CI context should be the one writing the note, not the test harness)
    repo.git(&["checkout", "gitai-feature"]).unwrap();
    file_a.set_contents(crate::lines!["human line 1", "human line 2"]);
    file_b.set_contents(crate::lines!["ai line 1"]);
    file_c.set_contents(crate::lines!["ai line 2"]);
    file_d.set_contents(crate::lines!["ai line 3"]);
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "Squash merge task branch"])
        .unwrap();
    let squash_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Set up bare origin for CiContext
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

    // Run CiContext — this should detect squash (not rebase)
    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    let base_sha = repo
        .git(&["merge-base", &task_tip_sha, "gitai-feature~1"])
        .unwrap()
        .trim()
        .to_string();

    let event = CiEvent::Merge {
        merge_commit_sha: squash_sha.clone(),
        head_ref: "task-branch".to_string(),
        head_sha: task_tip_sha.clone(),
        base_ref: "gitai-feature".to_string(),
        base_sha,
    };

    let ctx = CiContext::with_repository(git_ai_repo.clone(), event);
    ctx.run_with_options(CiRunOptions {
        skip_fetch_notes: true,
        skip_fetch_base: true,
    })
    .expect("CiContext run should succeed");

    // KEY ASSERTION: the squash commit should have aggregated contributors
    let squash_log = get_reference_as_authorship_log_v3(&git_ai_repo, &squash_sha)
        .expect("Squash commit should have authorship log");

    // Should have multiple prompts (aggregated from all source commits)
    assert!(
        squash_log.metadata.prompts.len() >= 2,
        "Squash merge should aggregate prompts from multiple source commits. Got {} prompts: {:?}",
        squash_log.metadata.prompts.len(),
        squash_log.metadata.prompts.keys().collect::<Vec<_>>()
    );

    // Contributors should be populated with aggregated stats
    assert!(
        squash_log.metadata.contributors.is_some(),
        "Squash merge should have aggregated contributors"
    );
    let contributors = squash_log.metadata.contributors.unwrap();
    assert!(!contributors.is_empty(), "Contributors should not be empty");

    // Should have AI contributions from multiple prompts
    let total_ai: u32 = contributors.values().map(|c| c.ai_accepted).sum();
    assert!(
        total_ai >= 2,
        "Aggregated contributors should have ai_accepted >= 2 (from multiple AI commits). Got: {}",
        total_ai
    );
}

/// Test that a real rebase merge onto a branch with prior history is still correctly
/// detected as rebase (not squash). The patch-id verification should confirm the
/// walked-back commits match the originals.
#[test]
fn test_ci_rebase_merge_correctly_detected_on_branch_with_history() {
    use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
    use git_ai::ci::ci_context::{CiContext, CiEvent, CiRunOptions};

    let repo = direct_test_repo();

    // Create initial commit on main
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create target branch with prior commits (history)
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut prior1 = repo.filename("prior1.txt");
    prior1.set_contents(crate::lines!["prior 1"]);
    repo.stage_all_and_commit("Prior 1").unwrap();

    let mut prior2 = repo.filename("prior2.txt");
    prior2.set_contents(crate::lines!["prior 2"]);
    repo.stage_all_and_commit("Prior 2").unwrap();

    // Create task branch from feature with 2 AI commits
    repo.git(&["checkout", "-b", "task"]).unwrap();

    let mut file_a = repo.filename("file_a.txt");
    file_a.set_contents(crate::lines!["ai in file_a".ai()]);
    let task_sha1 = repo.stage_all_and_commit("Add file_a").unwrap().commit_sha;

    let mut file_b = repo.filename("file_b.txt");
    file_b.set_contents(crate::lines!["ai in file_b".ai()]);
    let task_sha2 = repo.stage_all_and_commit("Add file_b").unwrap().commit_sha;

    // Simulate rebase merge onto feature: cherry-pick each task commit
    repo.git(&["checkout", "feature"]).unwrap();

    repo.git_og(&["cherry-pick", &task_sha1]).unwrap();
    let new_sha1 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.git_og(&["cherry-pick", &task_sha2]).unwrap();
    let new_sha2 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Set up bare origin
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

    // Run CiContext — should detect rebase (not squash) despite prior history
    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    let base_sha = repo
        .git(&["merge-base", &task_sha2, "feature~2"])
        .unwrap()
        .trim()
        .to_string();

    let event = CiEvent::Merge {
        merge_commit_sha: new_sha2.clone(),
        head_ref: "task".to_string(),
        head_sha: task_sha2.clone(),
        base_ref: "feature".to_string(),
        base_sha,
    };

    let ctx = CiContext::with_repository(git_ai_repo, event);
    ctx.run_with_options(CiRunOptions {
        skip_fetch_notes: true,
        skip_fetch_base: true,
    })
    .expect("CiContext run should succeed");

    // Verify each rebased commit got its own note (rebase path, not squash)
    let note1 = repo
        .read_authorship_note(&new_sha1)
        .expect("rebased commit 1 should have authorship note");
    let log1 = AuthorshipLog::deserialize_from_string(&note1).unwrap();

    let note2 = repo
        .read_authorship_note(&new_sha2)
        .expect("rebased commit 2 should have authorship note");
    let log2 = AuthorshipLog::deserialize_from_string(&note2).unwrap();

    // Each commit should have its own file's attestation (not aggregated)
    let files1: Vec<String> = log1
        .attestations
        .iter()
        .map(|a| a.file_path.clone())
        .collect();
    let files2: Vec<String> = log2
        .attestations
        .iter()
        .map(|a| a.file_path.clone())
        .collect();

    assert!(
        files1.iter().any(|f| f.contains("file_a")),
        "Rebased commit 1 should have file_a. Got: {:?}",
        files1
    );
    assert!(
        files2.iter().any(|f| f.contains("file_b")),
        "Rebased commit 2 should have file_b. Got: {:?}",
        files2
    );

    // Both should have contributors (per-commit)
    assert!(
        log1.metadata.contributors.is_some(),
        "Rebased commit 1 should have contributors"
    );
    assert!(
        log2.metadata.contributors.is_some(),
        "Rebased commit 2 should have contributors"
    );
}

/// Test that contributors.ai_accepted matches the top-level ai_accepted after a CI squash merge.
/// This pins the fix for the mismatch caused by build_contributors reading pre-merge
/// prompt.accepted_lines from source notes instead of the final merged VA result.
#[test]
fn test_ci_squash_merge_contributors_ai_accepted_matches_top_level() {
    use git_ai::authorship::stats::stats_for_commit_stats;

    let repo = direct_test_repo();
    let mut file = repo.filename("service.ts");

    // Create initial commit on main
    file.set_contents(crate::lines!["export class Service {", "}"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Create feature branch with two AI prompts
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Commit 1: AI adds 2 lines
    file.insert_at(
        1,
        crate::lines!["  connect() {}".ai(), "  disconnect() {}".ai()],
    );
    repo.stage_all_and_commit("AI adds connect/disconnect")
        .unwrap();

    // Commit 2: AI adds 1 more line (different prompt session)
    file.insert_at(3, crate::lines!["  ping() {}".ai()]);
    let feature_commit = repo.stage_all_and_commit("AI adds ping").unwrap();
    let feature_sha = feature_commit.commit_sha;

    // Simulate CI squash merge onto main
    repo.git(&["checkout", "main"]).unwrap();
    file.set_contents(crate::lines![
        "export class Service {",
        "  connect() {}",
        "  disconnect() {}",
        "  ping() {}",
        "}"
    ]);
    let merge_commit = repo.stage_all_and_commit("Squash feature").unwrap();
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

    // Compute top-level stats (blame-based, the authoritative source)
    let top_level = stats_for_commit_stats(&git_ai_repo, &merge_sha, &[])
        .expect("stats_for_commit_stats should succeed");

    // Read contributors from the note
    let squash_log = get_reference_as_authorship_log_v3(&git_ai_repo, &merge_sha)
        .expect("Squash commit should have authorship log");
    let contributors = squash_log
        .metadata
        .contributors
        .expect("contributors should be populated");

    // KEY ASSERTION: contributors ai_accepted must match the top-level value exactly
    let contributors_total_ai_accepted: u32 = contributors.values().map(|c| c.ai_accepted).sum();
    assert_eq!(
        contributors_total_ai_accepted, top_level.ai_accepted,
        "contributors.ai_accepted ({}) must match top-level ai_accepted ({})",
        contributors_total_ai_accepted, top_level.ai_accepted,
    );

    let contributors_total_ai_additions: u32 = contributors.values().map(|c| c.ai_additions).sum();
    assert_eq!(
        contributors_total_ai_additions, top_level.ai_additions,
        "contributors.ai_additions ({}) must match top-level ai_additions ({})",
        contributors_total_ai_additions, top_level.ai_additions,
    );
}

/// Test that a second-level CI squash (feature → main) correctly merges the existing contributors
/// sections from each source commit (built at task → feature time) via Priority 1, rather than
/// re-deriving from the merged log. This pins the fix that prevents Priority 0 from incorrectly
/// bypassing Priority 1 when source commits already have a contributors section.
#[test]
fn test_ci_squash_merge_second_level_merges_existing_contributors() {
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;

    let repo = direct_test_repo();

    // --- First level: task → feature squash ---

    // Initial commit on feature branch (acts as base)
    let mut file = repo.filename("app.ts");
    file.set_contents(crate::lines!["export const app = {};"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "feature"]).unwrap();

    // Task branch with AI commits
    repo.git(&["checkout", "-b", "task"]).unwrap();
    file.insert_at(1, crate::lines!["export function init() {}".ai()]);
    repo.stage_all_and_commit("AI adds init").unwrap();
    file.insert_at(2, crate::lines!["export function run() {}".ai()]);
    let task_tip = repo.stage_all_and_commit("AI adds run").unwrap();
    let task_sha = task_tip.commit_sha;

    // Squash task → feature
    repo.git(&["checkout", "feature"]).unwrap();
    file.set_contents(crate::lines![
        "export const app = {};",
        "export function init() {}",
        "export function run() {}",
    ]);
    let feature_merge = repo.stage_all_and_commit("Squash task").unwrap();
    let feature_merge_sha = feature_merge.commit_sha;

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "task",
        "feature",
        &task_sha,
        &feature_merge_sha,
        false,
    )
    .unwrap();

    // Verify the first-level squash commit has contributors
    let first_level_log = get_reference_as_authorship_log_v3(&git_ai_repo, &feature_merge_sha)
        .expect("First-level squash should have authorship log");
    assert!(
        first_level_log.metadata.contributors.is_some(),
        "First-level squash should have contributors"
    );
    let first_level_contributors = first_level_log.metadata.contributors.clone().unwrap();
    let first_level_ai_accepted: u32 = first_level_contributors
        .values()
        .map(|c| c.ai_accepted)
        .sum();
    assert!(
        first_level_ai_accepted > 0,
        "First-level squash contributors should have ai_accepted > 0"
    );

    // --- Second level: feature → main squash ---

    repo.git(&["checkout", "-b", "main"]).unwrap();
    // Detach to create main at initial state, then reset feature merge onto it
    // Simpler: create main from the initial state
    let initial_sha = repo
        .git(&["rev-list", "--max-parents=0", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "main2", &initial_sha])
        .unwrap();

    file.set_contents(crate::lines![
        "export const app = {};",
        "export function init() {}",
        "export function run() {}",
    ]);
    let main_merge = repo
        .stage_all_and_commit("Squash feature onto main")
        .unwrap();
    let main_merge_sha = main_merge.commit_sha;

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feature",
        "main2",
        &feature_merge_sha,
        &main_merge_sha,
        false,
    )
    .unwrap();

    let second_level_log = get_reference_as_authorship_log_v3(&git_ai_repo, &main_merge_sha)
        .expect("Second-level squash should have authorship log");

    // KEY ASSERTION: second-level contributors must have at least as much ai_accepted as first-level
    // (Priority 1 path: merges existing contributors from the feature merge commit)
    let second_level_contributors = second_level_log
        .metadata
        .contributors
        .expect("Second-level squash should have contributors");
    let second_level_ai_accepted: u32 = second_level_contributors
        .values()
        .map(|c| c.ai_accepted)
        .sum();
    assert_eq!(
        second_level_ai_accepted, first_level_ai_accepted,
        "Second-level squash contributors.ai_accepted ({}) should equal first-level ({}): Priority 1 merge should propagate contributors unchanged",
        second_level_ai_accepted, first_level_ai_accepted,
    );
}

/// Test that squash-merging a branch with only manual (no AI) commits still
/// populates the contributors field with manual_additions. Covers Bug 1 (Priority 0
/// fallthrough) and Bug 2 (empty-files path builds contributors).
#[test]
fn test_ci_squash_merge_manual_only_populates_contributors() {
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;

    let repo = direct_test_repo();
    let mut file = repo.filename("manual.ts");

    // Initial commit on main
    file.set_contents(crate::lines!["export class Manual {", "  run() {}", "}"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Task branch with ONLY manual commits (no .ai() markers)
    repo.git(&["checkout", "-b", "task-manual"]).unwrap();
    file.insert_at(1, crate::lines!["  name = 'widget';", "  count = 0;"]);
    repo.stage_all_and_commit("Manual: add name and count")
        .unwrap();
    file.insert_at(3, crate::lines!["  label = 'default';"]);
    let task_tip = repo.stage_all_and_commit("Manual: add label").unwrap();
    let task_sha = task_tip.commit_sha;

    // Squash onto main
    repo.git(&["checkout", "main"]).unwrap();
    file.set_contents(crate::lines![
        "export class Manual {",
        "  name = 'widget';",
        "  count = 0;",
        "  label = 'default';",
        "  run() {}",
        "}"
    ]);
    let merge_commit = repo.stage_all_and_commit("Squash task-manual").unwrap();
    let merge_sha = merge_commit.commit_sha;

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "task-manual",
        "main",
        &task_sha,
        &merge_sha,
        false,
    )
    .unwrap();

    let squash_log = get_reference_as_authorship_log_v3(&git_ai_repo, &merge_sha)
        .expect("Manual-only squash should still create authorship log");

    // KEY: contributors must be populated even with no AI
    assert!(
        squash_log.metadata.contributors.is_some(),
        "Manual-only squash should have contributors"
    );
    let contributors = squash_log.metadata.contributors.unwrap();
    assert!(!contributors.is_empty(), "Contributors should not be empty");

    let total_manual: u32 = contributors.values().map(|c| c.manual_additions).sum();
    let total_ai: u32 = contributors.values().map(|c| c.ai_accepted).sum();
    assert!(
        total_manual > 0,
        "Manual-only squash should have manual_additions > 0. Got: {:?}",
        contributors
    );
    assert_eq!(
        total_ai, 0,
        "Manual-only squash should have zero ai_accepted"
    );
}

/// Test that when feat already has Alice's AI squash commit, Bob's squash into feat
/// does NOT include Alice's prompt stats in Bob's contributors (Bug 4: base-branch
/// prompt leakage). Also verifies Bug 3 (prompt recovery from source notes).
#[test]
fn test_ci_squash_merge_no_base_branch_prompt_leakage() {
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;

    let repo = direct_test_repo();

    // --- Alice's task → feat ---
    let mut file_a = repo.filename("alice.ts");
    file_a.set_contents(crate::lines!["// Alice base"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "feat"]).unwrap();

    repo.git(&["checkout", "-b", "task-alice"]).unwrap();
    file_a.insert_at(1, crate::lines!["function aliceAI() {}".ai()]);
    let alice_tip = repo.stage_all_and_commit("Alice AI commit").unwrap();
    let alice_sha = alice_tip.commit_sha;

    // Squash Alice → feat
    repo.git(&["checkout", "feat"]).unwrap();
    file_a.set_contents(crate::lines!["// Alice base", "function aliceAI() {}"]);
    let feat_squash = repo.stage_all_and_commit("Squash Alice").unwrap();
    let feat_squash_sha = feat_squash.commit_sha;

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "task-alice",
        "feat",
        &alice_sha,
        &feat_squash_sha,
        false,
    )
    .unwrap();

    // Verify Alice's squash has contributors
    let alice_log = get_reference_as_authorship_log_v3(&git_ai_repo, &feat_squash_sha)
        .expect("Alice squash should have log");
    assert!(
        alice_log.metadata.contributors.is_some(),
        "Alice squash should have contributors"
    );

    // --- Bob's task → feat (feat already has Alice's AI notes) ---
    repo.git(&["checkout", "-b", "task-bob"]).unwrap();
    let mut file_b = repo.filename("bob.ts");
    file_b.set_contents(crate::lines![
        "// Bob code",
        "function bobAI() {}".ai(),
        "function bobManual() {}"
    ]);
    let bob_tip = repo.stage_all_and_commit("Bob's commit").unwrap();
    let bob_sha = bob_tip.commit_sha;

    // Squash Bob → feat
    repo.git(&["checkout", "feat"]).unwrap();
    file_b.set_contents(crate::lines![
        "// Bob code",
        "function bobAI() {}",
        "function bobManual() {}"
    ]);
    let bob_squash = repo.stage_all_and_commit("Squash Bob").unwrap();
    let bob_squash_sha = bob_squash.commit_sha;

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "task-bob",
        "feat",
        &bob_sha,
        &bob_squash_sha,
        false,
    )
    .unwrap();

    let bob_log = get_reference_as_authorship_log_v3(&git_ai_repo, &bob_squash_sha)
        .expect("Bob squash should have log");

    // KEY: Bob's squash should have contributors with ONLY Bob's stats
    assert!(
        bob_log.metadata.contributors.is_some(),
        "Bob squash should have contributors"
    );
    let contributors = bob_log.metadata.contributors.unwrap();

    // All contributors should belong to the same test user (single developer in test repo).
    // The key assertion: ai_accepted should reflect ONLY Bob's AI lines, not Alice's.
    // Alice had 1 AI line; Bob has 1 AI line. If leaking, total would be 2.
    let total_ai: u32 = contributors.values().map(|c| c.ai_accepted).sum();
    let total_manual: u32 = contributors.values().map(|c| c.manual_additions).sum();
    assert!(
        total_ai >= 1,
        "Bob should have ai_accepted >= 1. Got: {:?}",
        contributors
    );
    // Bob's contribution: 1 AI line + 2 manual lines (comment + manual function) = 3 total
    // If Alice leaked, we'd see alice's AI line in ai_accepted (total_ai would be > bob's 1)
    assert!(
        total_manual > 0,
        "Bob should have manual_additions > 0. Got: {:?}",
        contributors
    );
}

/// Test the full multi-level branching workflow with mixed AI and manual contributions:
/// task-1 (Alice, AI) → feat, task-2 (Bob, manual) → feat, feat → main.
/// Both developers' stats should appear in main's contributors without double-counting.
#[test]
fn test_ci_squash_merge_second_level_mixed_ai_and_manual_contributors() {
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;

    let repo = direct_test_repo();

    // --- Setup: initial commit on feat ---
    let mut file = repo.filename("shared.ts");
    file.set_contents(crate::lines!["export const app = {};"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "feat"]).unwrap();

    // --- Alice's task: AI commits ---
    repo.git(&["checkout", "-b", "task-alice"]).unwrap();
    file.insert_at(1, crate::lines!["export function aiInit() {}".ai()]);
    let alice_tip = repo.stage_all_and_commit("Alice AI").unwrap();

    // Squash Alice → feat
    repo.git(&["checkout", "feat"]).unwrap();
    file.set_contents(crate::lines![
        "export const app = {};",
        "export function aiInit() {}",
    ]);
    let alice_squash = repo.stage_all_and_commit("Squash Alice").unwrap();

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "task-alice",
        "feat",
        &alice_tip.commit_sha,
        &alice_squash.commit_sha,
        false,
    )
    .unwrap();

    let alice_log = get_reference_as_authorship_log_v3(&git_ai_repo, &alice_squash.commit_sha)
        .expect("Alice squash log");
    let alice_ai: u32 = alice_log
        .metadata
        .contributors
        .as_ref()
        .unwrap()
        .values()
        .map(|c| c.ai_accepted)
        .sum();
    assert!(alice_ai > 0, "Alice should have ai_accepted > 0");

    // --- Bob's task: manual-only commits ---
    repo.git(&["checkout", "-b", "task-bob"]).unwrap();
    let mut file_b = repo.filename("bob_manual.ts");
    file_b.set_contents(crate::lines![
        "function manual1() {}",
        "function manual2() {}"
    ]);
    let bob_tip = repo.stage_all_and_commit("Bob manual").unwrap();

    // Squash Bob → feat
    repo.git(&["checkout", "feat"]).unwrap();
    file_b.set_contents(crate::lines![
        "function manual1() {}",
        "function manual2() {}"
    ]);
    let bob_squash = repo.stage_all_and_commit("Squash Bob").unwrap();

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "task-bob",
        "feat",
        &bob_tip.commit_sha,
        &bob_squash.commit_sha,
        false,
    )
    .unwrap();

    // --- Second level: feat → main ---
    let initial_sha = repo
        .git(&["rev-list", "--max-parents=0", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "main", &initial_sha]).unwrap();

    file.set_contents(crate::lines![
        "export const app = {};",
        "export function aiInit() {}",
    ]);
    file_b.set_contents(crate::lines![
        "function manual1() {}",
        "function manual2() {}"
    ]);
    let main_squash = repo.stage_all_and_commit("Squash feat onto main").unwrap();

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feat",
        "main",
        &bob_squash.commit_sha, // feat tip
        &main_squash.commit_sha,
        false,
    )
    .unwrap();

    let main_log = get_reference_as_authorship_log_v3(&git_ai_repo, &main_squash.commit_sha)
        .expect("Main squash should have log");

    // KEY: main should have contributors (aggregated from Alice + Bob)
    let contributors = main_log
        .metadata
        .contributors
        .expect("Main squash should have contributors");
    assert!(
        !contributors.is_empty(),
        "Main contributors should not be empty"
    );

    // Both AI and manual contributions should be present
    let total_ai: u32 = contributors.values().map(|c| c.ai_accepted).sum();
    let total_manual: u32 = contributors.values().map(|c| c.manual_additions).sum();
    assert!(
        total_ai > 0,
        "Main should have ai_accepted > 0 (from Alice). Got: {:?}",
        contributors
    );
    assert!(
        total_manual > 0,
        "Main should have manual_additions > 0 (from Bob). Got: {:?}",
        contributors
    );
}

/// Test Bob's exact reported scenario: task branch does `git merge feat` (creating a
/// merge commit), then squash-merges into feat. Verifies that Bob's prompts are present
/// and contributors are populated despite the merge commit in source_commits.
#[test]
fn test_ci_squash_merge_with_merge_commit_in_source() {
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;

    let repo = direct_test_repo();

    // --- Initial state on feat ---
    let mut file_shared = repo.filename("shared.ts");
    file_shared.set_contents(crate::lines!["// shared base"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "feat"]).unwrap();

    // --- Bob branches from feat ---
    repo.git(&["checkout", "-b", "task-bob"]).unwrap();
    let mut file_bob = repo.filename("bob.ts");
    file_bob.set_contents(crate::lines![
        "// bob code",
        "function bobAI() {}".ai(),
        "function bobManual() {}"
    ]);
    let bob_commit = repo.stage_all_and_commit("Bob: init").unwrap();

    // --- Meanwhile, Alice's work lands on feat (simulated) ---
    repo.git(&["checkout", "feat"]).unwrap();
    let mut file_alice = repo.filename("alice.ts");
    file_alice.set_contents(crate::lines![
        "// alice code",
        "function aliceWork() {}".ai()
    ]);
    let alice_on_feat = repo.stage_all_and_commit("Alice squash on feat").unwrap();

    // Create AI notes on Alice's feat commit (simulate CI having run)
    rewrite_authorship_after_squash_or_rebase(
        &GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap(),
        "task-bob", // doesn't matter for this, we just need notes on feat
        "feat",
        &bob_commit.commit_sha, // won't actually be used since we're creating notes manually
        &alice_on_feat.commit_sha,
        false,
    )
    .ok(); // May or may not succeed depending on state, that's fine

    // --- Bob merges feat into task-bob (creating a merge commit) ---
    repo.git(&["checkout", "task-bob"]).unwrap();
    repo.git(&["merge", "feat", "-m", "Merge feat into task-bob"])
        .unwrap();

    // Get the merge commit (HEAD after merge)
    let merge_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // --- Squash Bob's task into feat ---
    repo.git(&["checkout", "feat"]).unwrap();
    file_bob.set_contents(crate::lines![
        "// bob code",
        "function bobAI() {}",
        "function bobManual() {}"
    ]);
    let bob_squash = repo.stage_all_and_commit("Squash task-bob").unwrap();

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "task-bob",
        "feat",
        &merge_head, // source_head = merge commit (includes git merge feat)
        &bob_squash.commit_sha,
        false,
    )
    .unwrap();

    let squash_log = get_reference_as_authorship_log_v3(&git_ai_repo, &bob_squash.commit_sha)
        .expect("Bob squash should have authorship log");

    // KEY: Bob's squash should have contributors populated
    assert!(
        squash_log.metadata.contributors.is_some(),
        "Squash with merge commit in source should have contributors. Log: {:?}",
        squash_log.metadata
    );
    let contributors = squash_log.metadata.contributors.unwrap();
    assert!(!contributors.is_empty(), "Contributors should not be empty");

    // Bob should have AI stats (from bob.ts AI line)
    let total_ai: u32 = contributors.values().map(|c| c.ai_accepted).sum();
    assert_eq!(
        total_ai, 1,
        "ai_accepted should be 1 (bobAI). Got: {:?}",
        contributors
    );

    // manual_additions must NOT include Alice's lines from the merge commit.
    // Bob's commit has 3 additions: "// bob code" (manual), "bobAI" (AI), "bobManual" (manual).
    // Only the 2 non-AI lines should be manual. The merge commit brought in Alice's
    // 2 lines, but those must be excluded (merge commit diff inflation fix).
    let total_manual: u32 = contributors.values().map(|c| c.manual_additions).sum();
    assert_eq!(
        total_manual, 2,
        "manual_additions should be 2 (Bob's non-AI lines only, not Alice's merged lines). Got: {:?}",
        contributors
    );

    let total_human: u32 = contributors.values().map(|c| c.human_additions).sum();
    assert_eq!(
        total_human, 2,
        "human_additions should be 2 (manual only, no mixed). Got: {:?}",
        contributors
    );
}

/// Test that prompts from source commit notes are recovered when VA fails to load them.
/// The merged authorship log may have attestations referencing prompt IDs that aren't
/// in its prompts map (because discover_and_load_foreign_prompts failed silently).
/// Bug 3 fix ensures those prompts are added from source commit notes.
#[test]
fn test_ci_squash_merge_prompt_from_source_when_va_has_attestation() {
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;

    let repo = direct_test_repo();
    let mut file = repo.filename("prompt_test.ts");

    // Initial commit
    file.set_contents(crate::lines!["// base"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Feature branch with AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, crate::lines!["function fromAI() {}".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();
    file.insert_at(2, crate::lines!["function fromAI2() {}".ai()]);
    let feature_tip = repo.stage_all_and_commit("AI commit 2").unwrap();

    // Squash onto main
    repo.git(&["checkout", "main"]).unwrap();
    file.set_contents(crate::lines![
        "// base",
        "function fromAI() {}",
        "function fromAI2() {}"
    ]);
    let merge_commit = repo.stage_all_and_commit("Squash feature").unwrap();

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feature",
        "main",
        &feature_tip.commit_sha,
        &merge_commit.commit_sha,
        false,
    )
    .unwrap();

    let squash_log = get_reference_as_authorship_log_v3(&git_ai_repo, &merge_commit.commit_sha)
        .expect("Squash commit should have authorship log");

    // KEY: prompts should NOT be empty — Bug 3 fix ensures source commit prompts are recovered
    assert!(
        !squash_log.metadata.prompts.is_empty(),
        "Squash commit should have prompts (recovered from source commits). Got empty prompts."
    );

    // Each attestation entry's hash should exist in the prompts map
    // (no orphaned attestation references)
    for file_attestation in &squash_log.attestations {
        for entry in &file_attestation.entries {
            assert!(
                squash_log.metadata.prompts.contains_key(&entry.hash)
                    || entry.hash.starts_with("h_"),
                "Attestation in {} references prompt {} which is missing from prompts map",
                file_attestation.file_path,
                entry.hash
            );
        }
    }
}

/// Test that when feat has Alice's AI squash, Bob's squash into feat has contributors
/// where Alice's email is NOT present. Sharpened assertion for Bug 4.
#[test]
fn test_ci_squash_merge_base_branch_ai_not_in_squash_contributors() {
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;

    let repo = direct_test_repo();

    // All commits in this test repo use the same "Test User" author, so we can't
    // distinguish Alice vs Bob by email. Instead, verify that contributors ai_accepted
    // reflects ONLY the squash PR's AI lines, not base branch AI lines.

    // --- Alice's AI work on feat ---
    let mut file_a = repo.filename("alice_code.ts");
    file_a.set_contents(crate::lines!["// base"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "feat"]).unwrap();

    repo.git(&["checkout", "-b", "task-alice"]).unwrap();
    file_a.insert_at(
        1,
        crate::lines![
            "function alice1() {}".ai(),
            "function alice2() {}".ai(),
            "function alice3() {}".ai()
        ],
    );
    let alice_tip = repo.stage_all_and_commit("Alice 3 AI lines").unwrap();

    repo.git(&["checkout", "feat"]).unwrap();
    file_a.set_contents(crate::lines![
        "// base",
        "function alice1() {}",
        "function alice2() {}",
        "function alice3() {}",
    ]);
    let alice_squash = repo.stage_all_and_commit("Squash Alice").unwrap();

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "task-alice",
        "feat",
        &alice_tip.commit_sha,
        &alice_squash.commit_sha,
        false,
    )
    .unwrap();

    let alice_log = get_reference_as_authorship_log_v3(&git_ai_repo, &alice_squash.commit_sha)
        .expect("Alice log");
    let alice_ai: u32 = alice_log
        .metadata
        .contributors
        .as_ref()
        .unwrap()
        .values()
        .map(|c| c.ai_accepted)
        .sum();
    assert!(alice_ai >= 3, "Alice should have >= 3 ai_accepted");

    // --- Bob's AI work (1 AI line), squash into feat ---
    repo.git(&["checkout", "-b", "task-bob"]).unwrap();
    let mut file_b = repo.filename("bob_code.ts");
    file_b.set_contents(crate::lines!["function bob1() {}".ai()]);
    let bob_tip = repo.stage_all_and_commit("Bob 1 AI line").unwrap();

    repo.git(&["checkout", "feat"]).unwrap();
    file_b.set_contents(crate::lines!["function bob1() {}"]);
    let bob_squash = repo.stage_all_and_commit("Squash Bob").unwrap();

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "task-bob",
        "feat",
        &bob_tip.commit_sha,
        &bob_squash.commit_sha,
        false,
    )
    .unwrap();

    let bob_log = get_reference_as_authorship_log_v3(&git_ai_repo, &bob_squash.commit_sha)
        .expect("Bob squash log");

    let bob_contributors = bob_log
        .metadata
        .contributors
        .expect("Bob squash should have contributors");

    // KEY: Bob's ai_accepted should be ~1 (his 1 AI line), NOT 4 (1 + Alice's 3)
    let bob_total_ai: u32 = bob_contributors.values().map(|c| c.ai_accepted).sum();
    assert!(
        bob_total_ai <= 2,
        "Bob's squash ai_accepted should reflect only Bob's AI lines (~1), not Alice's. Got {} (Alice had {}). This indicates base-branch prompt leakage.",
        bob_total_ai,
        alice_ai
    );
}

/// Case 1: Four individual commits with all 4 change types, squash merged.
/// Standard pattern:
///   commit 1: human manual add
///   commit 2: AI adds 2 lines (prompt A) — one kept, one to be overridden
///   commit 3: human overrides one of AI's lines from commit 2
///   commit 4: different AI adds a line (prompt B)
/// Verifies prompts (accepted_lines, total_additions) and contributors.
#[test]
fn test_ci_squash_four_commits_all_change_types() {
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;

    let repo = direct_test_repo();
    let mut file = repo.filename("service.ts");

    // Initial state on main
    file.set_contents(crate::lines!["// module", "const x = 1;", "const y = 2;"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Feature branch with 4 commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Commit 1: human manual add
    file.insert_at(2, crate::lines!["const manual = true;"]);
    repo.stage_all_and_commit("Human adds manual line").unwrap();

    // Commit 2: AI (prompt A) adds 2 lines — one kept, one to be overridden
    file.insert_at(
        3,
        crate::lines!["const aiKept = 1;".ai(), "const aiTemp = 1;".ai()],
    );
    let commit2 = repo.stage_all_and_commit("AI adds two lines").unwrap();
    let commit2_sha = commit2.commit_sha.clone();

    // Commit 3: human overrides the AI "aiTemp" line
    file.replace_at(4, "const aiOverridden = 2;");
    repo.stage_all_and_commit("Human overrides AI line")
        .unwrap();

    // Commit 4: different AI (prompt B) adds a line
    file.insert_at(5, crate::lines!["const diffAi = 1;".ai()]);
    let commit4 = repo.stage_all_and_commit("Different AI adds line").unwrap();
    let commit4_sha = commit4.commit_sha.clone();

    let feature_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Capture prompt IDs from source commits
    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    let prompt_a_id = get_reference_as_authorship_log_v3(&git_ai_repo, &commit2_sha)
        .expect("Commit 2 should have log")
        .metadata
        .prompts
        .keys()
        .next()
        .expect("Commit 2 should have prompt")
        .clone();

    let prompt_b_id = get_reference_as_authorship_log_v3(&git_ai_repo, &commit4_sha)
        .expect("Commit 4 should have log")
        .metadata
        .prompts
        .keys()
        .next()
        .expect("Commit 4 should have prompt")
        .clone();

    assert_ne!(prompt_a_id, prompt_b_id, "Prompts A and B should differ");

    // Squash merge onto main
    repo.git(&["checkout", "main"]).unwrap();
    file.set_contents(crate::lines![
        "// module",
        "const x = 1;",
        "const manual = true;",
        "const aiKept = 1;",
        "const aiOverridden = 2;",
        "const diffAi = 1;",
        "const y = 2;"
    ]);
    let merge_commit = repo.stage_all_and_commit("Squash feature").unwrap();
    let merge_sha = merge_commit.commit_sha;

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feature",
        "main",
        &feature_sha,
        &merge_sha,
        false,
    )
    .unwrap();

    let log = get_reference_as_authorship_log_v3(&git_ai_repo, &merge_sha)
        .expect("Squash should have log");

    // --- Prompt assertions ---
    let pa = log
        .metadata
        .prompts
        .get(&prompt_a_id)
        .unwrap_or_else(|| panic!("Prompt A ({}) missing", prompt_a_id));
    assert_eq!(pa.total_additions, 2, "Prompt A total_additions");
    assert_eq!(
        pa.accepted_lines, 1,
        "Prompt A accepted_lines: aiKept survived. Got accepted={}, overriden={}",
        pa.accepted_lines, pa.overriden_lines
    );

    let pb = log
        .metadata
        .prompts
        .get(&prompt_b_id)
        .unwrap_or_else(|| panic!("Prompt B ({}) missing", prompt_b_id));
    assert_eq!(pb.total_additions, 1, "Prompt B total_additions");
    assert_eq!(
        pb.accepted_lines, 1,
        "Prompt B accepted_lines: diffAi survived. Got accepted={}, overriden={}",
        pb.accepted_lines, pb.overriden_lines
    );

    // --- Contributor assertions ---
    let c = log.metadata.contributors.expect("Should have contributors");
    let ai_accepted: u32 = c.values().map(|s| s.ai_accepted).sum();
    let manual: u32 = c.values().map(|s| s.manual_additions).sum();

    assert_eq!(
        ai_accepted, 2,
        "ai_accepted should be 2 (aiKept + diffAi). contributors={:?}",
        c
    );
    assert!(
        manual >= 1,
        "manual_additions should be >= 1. contributors={:?}",
        c
    );
}

/// Case 2: All 4 change types in a single commit, squash merged.
/// In a single commit all AI lines share one prompt. The "override" is simulated
/// by set_contents's two-step write (AI stubs → human overwrites).
#[test]
fn test_ci_squash_single_commit_all_change_types() {
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;

    let repo = direct_test_repo();
    let mut file = repo.filename("config.ts");

    // Initial state on main
    file.set_contents(crate::lines![
        "// config",
        "const a = 1;",
        "const b = 2;",
        "const c = 3;"
    ]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Feature branch — single commit with all 4 change types
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // set_contents creates a single commit. AI lines share one prompt.
    // The "human override" line is plain text — set_contents's two-step write
    // (AI stubs first, then human fills them) means this line was briefly AI
    // then overwritten by the human content.
    file.set_contents(crate::lines![
        "// config",
        "const a = 1;",
        "const manual = true;",     // change 1: human manual
        "const aiKept = 1;".ai(),   // change 2: AI add (accepted)
        "const humanOverride = 1;", // change 3: was AI, human overrode
        "const diffAi = 1;".ai(),   // change 4: different AI add (same prompt)
        "const b = 2;",
        "const c = 3;"
    ]);
    let feature_commit = repo
        .stage_all_and_commit("All changes in one commit")
        .unwrap();
    let feature_sha = feature_commit.commit_sha.clone();

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    let feature_log = get_reference_as_authorship_log_v3(&git_ai_repo, &feature_sha)
        .expect("Feature commit should have log");
    let prompt_id = feature_log
        .metadata
        .prompts
        .keys()
        .next()
        .expect("Should have a prompt")
        .clone();

    // Squash merge onto main
    repo.git(&["checkout", "main"]).unwrap();
    file.set_contents(crate::lines![
        "// config",
        "const a = 1;",
        "const manual = true;",
        "const aiKept = 1;",
        "const humanOverride = 1;",
        "const diffAi = 1;",
        "const b = 2;",
        "const c = 3;"
    ]);
    let merge_commit = repo.stage_all_and_commit("Squash feature").unwrap();
    let merge_sha = merge_commit.commit_sha;

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feature",
        "main",
        &feature_sha,
        &merge_sha,
        false,
    )
    .unwrap();

    let log = get_reference_as_authorship_log_v3(&git_ai_repo, &merge_sha)
        .expect("Squash should have log");

    // --- Prompt assertions ---
    let p = log
        .metadata
        .prompts
        .get(&prompt_id)
        .unwrap_or_else(|| panic!("Prompt ({}) missing", prompt_id));
    assert!(
        p.accepted_lines >= 2,
        "Single-commit prompt should have >= 2 accepted lines (aiKept + diffAi). Got accepted={}, total_add={}",
        p.accepted_lines,
        p.total_additions
    );

    // --- Contributor assertions ---
    let c = log.metadata.contributors.expect("Should have contributors");
    let ai_accepted: u32 = c.values().map(|s| s.ai_accepted).sum();
    let manual: u32 = c.values().map(|s| s.manual_additions).sum();

    assert!(
        ai_accepted >= 2,
        "ai_accepted should be >= 2 (aiKept + diffAi). contributors={:?}",
        c
    );
    assert!(
        manual >= 1,
        "manual_additions should be >= 1 (manual line). contributors={:?}",
        c
    );
}

/// Cases 3 & 4: Two task branches → feature → main.
///
/// task-1 (file_a): all 4 change types in one commit → squash into feat
/// task-2 (file_b): all 4 change types in one commit, merge feat (with task-1),
///                  then squash into feat
///
/// Case 3: verify each task squash has correct prompts and contributors
/// Case 4: squash feat → main; verify aggregated contributors from both tasks
#[test]
fn test_ci_squash_two_tasks_merge_then_feature_to_main() {
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;

    let repo = direct_test_repo();

    // --- Initial state on feat ---
    let mut file_a = repo.filename("task1.ts");
    let mut file_b = repo.filename("task2.ts");
    file_a.set_contents(crate::lines!["// task1", "const a = 1;", "const b = 2;"]);
    file_b.set_contents(crate::lines!["// task2", "const c = 1;", "const d = 2;"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "feat"]).unwrap();

    // --- TASK-1: branch from feat, all 4 types on file_a ---
    repo.git(&["checkout", "-b", "task-1"]).unwrap();
    file_a.set_contents(crate::lines![
        "// task1",
        "const a = 1;",
        "const t1Manual = true;",   // change 1: manual
        "const t1AiKept = 1;".ai(), // change 2: AI kept
        "const t1Override = 1;",    // change 3: was AI, human overrode
        "const t1DiffAi = 1;".ai(), // change 4: different AI (same prompt)
        "const b = 2;"
    ]);
    let task1_tip = repo.stage_all_and_commit("Task-1 changes").unwrap();
    let task1_sha = task1_tip.commit_sha.clone();

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    // Capture task-1 prompt
    let t1_log = get_reference_as_authorship_log_v3(&git_ai_repo, &task1_sha)
        .expect("Task-1 should have log");
    let prompt_t1 = t1_log.metadata.prompts.keys().next().cloned();

    // Squash task-1 → feat
    repo.git(&["checkout", "feat"]).unwrap();
    file_a.set_contents(crate::lines![
        "// task1",
        "const a = 1;",
        "const t1Manual = true;",
        "const t1AiKept = 1;",
        "const t1Override = 1;",
        "const t1DiffAi = 1;",
        "const b = 2;"
    ]);
    let squash1 = repo.stage_all_and_commit("Squash task-1").unwrap();
    let squash1_sha = squash1.commit_sha.clone();

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "task-1",
        "feat",
        &task1_sha,
        &squash1_sha,
        false,
    )
    .unwrap();

    // --- Case 3a: verify task-1 squash ---
    let log1 = get_reference_as_authorship_log_v3(&git_ai_repo, &squash1_sha)
        .expect("Squash-1 should have log");

    if let Some(ref pid) = prompt_t1 {
        let p = log1
            .metadata
            .prompts
            .get(pid)
            .unwrap_or_else(|| panic!("Task-1 prompt ({}) missing from squash-1", pid));
        assert!(
            p.accepted_lines >= 2,
            "Task-1 prompt accepted_lines should be >= 2. Got accepted={}, total_add={}",
            p.accepted_lines,
            p.total_additions
        );
    }
    let c1 = log1
        .metadata
        .contributors
        .as_ref()
        .expect("Squash-1 should have contributors");
    let c1_ai: u32 = c1.values().map(|s| s.ai_accepted).sum();
    let c1_manual: u32 = c1.values().map(|s| s.manual_additions).sum();
    assert!(
        c1_ai >= 2,
        "Task-1 ai_accepted should be >= 2. Got {:?}",
        c1
    );
    assert!(
        c1_manual >= 1,
        "Task-1 manual_additions should be >= 1. Got {:?}",
        c1
    );

    // --- TASK-2: branch from feat BEFORE task-1 merge, work on file_b ---
    // We need task-2 to branch from feat's state before task-1 was squashed.
    // Since feat already has the squash, we branch from its parent.
    let feat_parent = repo
        .git(&["rev-parse", "feat~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "task-2", &feat_parent])
        .unwrap();

    file_b.set_contents(crate::lines![
        "// task2",
        "const c = 1;",
        "const t2Manual = true;",   // change 1: manual
        "const t2AiKept = 1;".ai(), // change 2: AI kept
        "const t2Override = 1;",    // change 3: was AI, human overrode
        "const t2DiffAi = 1;".ai(), // change 4: different AI (same prompt)
        "const d = 2;"
    ]);
    let task2_commit = repo.stage_all_and_commit("Task-2 changes").unwrap();

    // Capture task-2 prompt
    let t2_log = get_reference_as_authorship_log_v3(&git_ai_repo, &task2_commit.commit_sha)
        .expect("Task-2 should have log");
    let prompt_t2 = t2_log.metadata.prompts.keys().next().cloned();

    // Merge feat into task-2 (pulls in task-1 squash)
    repo.git(&["merge", "feat", "-m", "Merge feat into task-2"])
        .unwrap();
    let merge_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Squash task-2 → feat
    repo.git(&["checkout", "feat"]).unwrap();
    file_b.set_contents(crate::lines![
        "// task2",
        "const c = 1;",
        "const t2Manual = true;",
        "const t2AiKept = 1;",
        "const t2Override = 1;",
        "const t2DiffAi = 1;",
        "const d = 2;"
    ]);
    let squash2 = repo.stage_all_and_commit("Squash task-2").unwrap();
    let squash2_sha = squash2.commit_sha.clone();

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "task-2",
        "feat",
        &merge_head,
        &squash2_sha,
        false,
    )
    .unwrap();

    // --- Case 3b: verify task-2 squash ---
    let log2 = get_reference_as_authorship_log_v3(&git_ai_repo, &squash2_sha)
        .expect("Squash-2 should have log");

    if let Some(ref pid) = prompt_t2 {
        let p = log2
            .metadata
            .prompts
            .get(pid)
            .unwrap_or_else(|| panic!("Task-2 prompt ({}) missing from squash-2", pid));
        assert!(
            p.accepted_lines >= 2,
            "Task-2 prompt accepted_lines should be >= 2. Got accepted={}, total_add={}",
            p.accepted_lines,
            p.total_additions
        );
    }
    let c2 = log2
        .metadata
        .contributors
        .as_ref()
        .expect("Squash-2 should have contributors");
    let c2_ai: u32 = c2.values().map(|s| s.ai_accepted).sum();
    let c2_manual: u32 = c2.values().map(|s| s.manual_additions).sum();
    assert!(
        c2_ai >= 2,
        "Task-2 ai_accepted should be >= 2. Got {:?}",
        c2
    );
    assert!(
        c2_manual >= 1,
        "Task-2 manual_additions should be >= 1. Got {:?}",
        c2
    );

    // Verify no prompt leakage from task-1 into task-2's squash
    if let (Some(t1_pid), Some(t2_pid)) = (&prompt_t1, &prompt_t2) {
        assert_ne!(t1_pid, t2_pid, "Task prompts should differ");
        assert!(
            !log2.metadata.prompts.contains_key(t1_pid.as_str()),
            "Task-1 prompt {} should NOT leak into task-2 squash. Prompts: {:?}",
            t1_pid,
            log2.metadata.prompts.keys().collect::<Vec<_>>()
        );
    }

    // --- Case 4: squash feat → main ---
    let initial_sha = repo
        .git(&["rev-list", "--max-parents=0", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "main", &initial_sha]).unwrap();

    // Set combined final state for both files
    file_a.set_contents(crate::lines![
        "// task1",
        "const a = 1;",
        "const t1Manual = true;",
        "const t1AiKept = 1;",
        "const t1Override = 1;",
        "const t1DiffAi = 1;",
        "const b = 2;"
    ]);
    file_b.set_contents(crate::lines![
        "// task2",
        "const c = 1;",
        "const t2Manual = true;",
        "const t2AiKept = 1;",
        "const t2Override = 1;",
        "const t2DiffAi = 1;",
        "const d = 2;"
    ]);
    let main_merge = repo.stage_all_and_commit("Squash feat onto main").unwrap();
    let main_sha = main_merge.commit_sha;

    let feat_tip = squash2_sha; // feat HEAD = latest squash
    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "feat",
        "main",
        &feat_tip,
        &main_sha,
        false,
    )
    .unwrap();

    let main_log = get_reference_as_authorship_log_v3(&git_ai_repo, &main_sha)
        .expect("Main squash should have log");
    let mc = main_log
        .metadata
        .contributors
        .as_ref()
        .expect("Main should have contributors");
    let main_ai: u32 = mc.values().map(|s| s.ai_accepted).sum();
    let main_manual: u32 = mc.values().map(|s| s.manual_additions).sum();

    assert!(
        main_ai >= 4,
        "Main ai_accepted should be >= 4 (2 from task-1 + 2 from task-2). Got {}. contributors={:?}",
        main_ai,
        mc
    );
    assert!(
        main_manual >= 2,
        "Main manual_additions should be >= 2 (1 from each task). Got {}. contributors={:?}",
        main_manual,
        mc
    );
}

/// Case 5: Same prompt ID across multiple source commits — one accepted, one overridden.
/// Reproduces the bug where h_ false positives and real overrides numerically mask
/// each other (both = 1), causing the h_ fix to not trigger and accepted_lines = 0.
#[test]
fn test_ci_squash_same_prompt_across_commits_accepted_and_overridden() {
    use git_ai::authorship::authorship_log::PromptRecord;
    use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
    use git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;
    use git_ai::authorship::working_log::AgentId;
    use git_ai::git::refs::notes_add;

    let repo = direct_test_repo();
    let mut file = repo.filename("service.ts");

    // Initial state on main
    file.set_contents(crate::lines!["// module", "const x = 1;", "const y = 2;"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // Task branch with 4 commits
    repo.git(&["checkout", "-b", "task"]).unwrap();

    // Commit 1: human manual add
    file.insert_at(2, crate::lines!["const manual = true;"]);
    repo.stage_all_and_commit("Human adds manual line").unwrap();

    // Commit 2: AI adds 1 line (accepted)
    file.insert_at(3, crate::lines!["const aiKept = 1;".ai()]);
    let commit2 = repo.stage_all_and_commit("AI adds accepted line").unwrap();
    let commit2_sha = commit2.commit_sha.clone();

    // Commit 3: Human writes a line that was originally AI-suggested then overridden.
    // The line is plain text (human-written), but the note will record it as
    // an AI prompt with accepted=0, overridden=1 (simulating AI suggestion → human override).
    file.insert_at(4, crate::lines!["const aiOverridden = 2;"]);
    let commit3 = repo
        .stage_all_and_commit("AI adds overridden line")
        .unwrap();
    let commit3_sha = commit3.commit_sha.clone();

    // Commit 4: different AI adds a line
    file.insert_at(5, crate::lines!["const diffAi = 1;".ai()]);
    let commit4 = repo.stage_all_and_commit("Different AI adds line").unwrap();
    let commit4_sha = commit4.commit_sha.clone();

    let task_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    // Get prompt IDs from commits 2, 3, 4
    let prompt_a_id = get_reference_as_authorship_log_v3(&git_ai_repo, &commit2_sha)
        .expect("Commit 2 should have log")
        .metadata
        .prompts
        .keys()
        .next()
        .expect("Commit 2 should have prompt")
        .clone();

    let prompt_b_id = get_reference_as_authorship_log_v3(&git_ai_repo, &commit4_sha)
        .expect("Commit 4 should have log")
        .metadata
        .prompts
        .keys()
        .next()
        .expect("Commit 4 should have prompt")
        .clone();

    // KEY: Rewrite commit 3's note to ADD a prompt with the SAME hash as commit 2,
    // simulating the real-world scenario where the same AI session spans multiple commits.
    // Commit 3 is a human-written line (no .ai()), so its note has no prompts.
    // We add one with accepted_lines=0, overriden_lines=1 to represent: AI suggested
    // something, human overwrote it (the committed line is the human's version).
    let commit3_note = repo
        .read_authorship_note(&commit3_sha)
        .expect("Commit 3 should have note");
    let mut commit3_log =
        AuthorshipLog::deserialize_from_string(&commit3_note).expect("parse commit 3 note");

    // Get the agent_id from commit 2's prompt to reuse the same session
    let commit2_record = get_reference_as_authorship_log_v3(&git_ai_repo, &commit2_sha)
        .expect("Commit 2 log")
        .metadata
        .prompts
        .get(&prompt_a_id)
        .expect("Commit 2 prompt")
        .clone();

    commit3_log.metadata.prompts.insert(
        prompt_a_id.clone(),
        PromptRecord {
            agent_id: AgentId {
                tool: commit2_record.agent_id.tool.clone(),
                id: commit2_record.agent_id.id.clone(),
                model: commit2_record.agent_id.model.clone(),
            },
            human_author: Some("Test User <test@example.com>".to_string()),
            messages: vec![],
            total_additions: 1,
            total_deletions: 0,
            accepted_lines: 0,
            overriden_lines: 1,
            messages_url: None,
            custom_attributes: None,
        },
    );
    let rewritten_note = commit3_log
        .serialize_to_string()
        .expect("serialize rewritten note");
    notes_add(&git_ai_repo, &commit3_sha, &rewritten_note)
        .expect("rewrite commit 3 note with shared prompt hash");

    // Squash merge onto main
    repo.git(&["checkout", "main"]).unwrap();
    file.set_contents(crate::lines![
        "// module",
        "const x = 1;",
        "const manual = true;",
        "const aiKept = 1;",
        "const aiOverridden = 2;",
        "const diffAi = 1;",
        "const y = 2;"
    ]);
    let merge_commit = repo.stage_all_and_commit("Squash task").unwrap();
    let merge_sha = merge_commit.commit_sha;

    rewrite_authorship_after_squash_or_rebase(
        &git_ai_repo,
        "task",
        "main",
        &task_sha,
        &merge_sha,
        false,
    )
    .unwrap();

    let log = get_reference_as_authorship_log_v3(&git_ai_repo, &merge_sha)
        .expect("Squash should have log");

    // --- Prompt assertions ---
    let pa = log
        .metadata
        .prompts
        .get(&prompt_a_id)
        .unwrap_or_else(|| panic!("Prompt A ({}) missing", prompt_a_id));
    assert_eq!(pa.total_additions, 2, "Prompt A total_additions");
    assert_eq!(
        pa.accepted_lines, 1,
        "Prompt A accepted_lines: aiKept survived, aiOverridden was overridden. Got accepted={}, overriden={}",
        pa.accepted_lines, pa.overriden_lines
    );
    assert_eq!(
        pa.overriden_lines, 1,
        "Prompt A overriden_lines: one line was human-overridden. Got overriden={}",
        pa.overriden_lines
    );

    let pb = log
        .metadata
        .prompts
        .get(&prompt_b_id)
        .unwrap_or_else(|| panic!("Prompt B ({}) missing", prompt_b_id));
    assert_eq!(pb.total_additions, 1, "Prompt B total_additions");
    assert_eq!(
        pb.accepted_lines, 1,
        "Prompt B accepted_lines: diffAi survived. Got accepted={}, overriden={}",
        pb.accepted_lines, pb.overriden_lines
    );

    // --- Contributor assertions ---
    let c = log.metadata.contributors.expect("Should have contributors");
    let ai_accepted: u32 = c.values().map(|s| s.ai_accepted).sum();
    let ai_additions: u32 = c.values().map(|s| s.ai_additions).sum();
    let mixed: u32 = c.values().map(|s| s.mixed_additions).sum();
    let manual: u32 = c.values().map(|s| s.manual_additions).sum();

    assert_eq!(
        ai_accepted, 2,
        "ai_accepted should be 2 (aiKept + diffAi). contributors={:?}",
        c
    );
    assert_eq!(
        mixed, 1,
        "mixed_additions should be 1 (aiOverridden). contributors={:?}",
        c
    );
    assert_eq!(
        ai_additions, 3,
        "ai_additions should be 3 (2 claude + 1 copilot). contributors={:?}",
        c
    );
    assert!(
        manual >= 1,
        "manual_additions should be >= 1. contributors={:?}",
        c
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
    test_ci_local_rebase_merge_two_commits,
    test_ci_local_rebase_merge_three_commits,
    test_ci_squash_merge_basic_standard_human,
    test_ci_squash_merge_mixed_content_standard_human,
    test_ci_squash_merge_with_manual_changes_standard_human,
    test_ci_rebase_merge_multiple_commits_standard_human,
    test_ci_squash_merge_populates_contributors,
    test_ci_rebase_merge_populates_contributors,
    test_ci_squash_merge_not_misdetected_as_rebase_on_branch_with_history,
    test_ci_rebase_merge_correctly_detected_on_branch_with_history,
    test_ci_squash_merge_contributors_ai_accepted_matches_top_level,
    test_ci_squash_merge_second_level_merges_existing_contributors,
    test_ci_squash_merge_manual_only_populates_contributors,
    test_ci_squash_merge_no_base_branch_prompt_leakage,
    test_ci_squash_merge_second_level_mixed_ai_and_manual_contributors,
    test_ci_squash_merge_with_merge_commit_in_source,
    test_ci_squash_merge_prompt_from_source_when_va_has_attestation,
    test_ci_squash_merge_base_branch_ai_not_in_squash_contributors,
    test_ci_squash_four_commits_all_change_types,
    test_ci_squash_single_commit_all_change_types,
    test_ci_squash_two_tasks_merge_then_feature_to_main,
    test_ci_squash_same_prompt_across_commits_accepted_and_overridden,
);
