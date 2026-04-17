use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// Guard test: after rebasing onto a branch with merge commits, the merge commits
/// on the target branch must NOT receive AI authorship notes.
///
/// This test uses the wrapper path where `onto_head` is correctly captured.
/// The unit test in rebase_hooks.rs tests the `onto_head = None` fallback path.
#[test]
fn test_rebase_onto_branch_with_merge_commits_does_not_note_merge_commits() {
    let repo = TestRepo::new();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(vec!["base line 1".human(), "base line 2".human()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit");

    let default_branch = repo.current_branch();

    // Create a merge commit on main via side branch
    repo.git(&["checkout", "-b", "side-branch"])
        .expect("create side branch");
    let mut side_file = repo.filename("side.txt");
    side_file.set_contents(vec!["side content".human()]);
    repo.stage_all_and_commit("side branch commit")
        .expect("side branch commit");

    repo.git(&["checkout", &default_branch])
        .expect("switch back to main");
    let mut main_file = repo.filename("main_extra.txt");
    main_file.set_contents(vec!["main extra content".human()]);
    repo.stage_all_and_commit("main commit before merge")
        .expect("main commit before merge");

    repo.git(&[
        "merge",
        "--no-ff",
        "side-branch",
        "-m",
        "Merge side-branch into main",
    ])
    .expect("merge side-branch");

    let merge_commit_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("get merge commit sha")
        .trim()
        .to_string();

    // Feature branch diverging from before the merge
    let pre_merge_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .expect("get pre-merge sha")
        .trim()
        .to_string();

    repo.git(&["checkout", "-b", "feature", &pre_merge_sha])
        .expect("create feature branch");

    let mut ai_file = repo.filename("ai_feature.txt");
    ai_file.set_contents(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);
    repo.stage_all_and_commit("add AI feature")
        .expect("AI feature commit");

    let feature_commit_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("get feature commit sha")
        .trim()
        .to_string();

    assert!(
        repo.read_authorship_note(&feature_commit_sha).is_some(),
        "AI feature commit should have an authorship note before rebase"
    );
    assert!(
        repo.read_authorship_note(&merge_commit_sha).is_none(),
        "Merge commit should NOT have an authorship note before rebase"
    );

    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed");

    let new_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("get new head")
        .trim()
        .to_string();

    assert_ne!(new_head, feature_commit_sha);

    // STRICT BLAME: AI file preserved
    ai_file.assert_lines_and_blame(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);

    // STRICT BLAME: human files untouched
    base_file.assert_lines_and_blame(vec!["base line 1".human(), "base line 2".human()]);
    main_file.assert_lines_and_blame(vec!["main extra content".human()]);
    side_file.assert_lines_and_blame(vec!["side content".human()]);

    let rebased_note = repo.read_authorship_note(&new_head);
    assert!(
        rebased_note.is_some(),
        "Rebased AI commit should have an authorship note"
    );
    assert!(
        rebased_note.unwrap().contains("ai_feature.txt"),
        "Rebased note should reference ai_feature.txt"
    );

    let merge_note_after = repo.read_authorship_note(&merge_commit_sha);
    assert!(
        merge_note_after.is_none(),
        "Merge commit on target branch should NOT have an authorship note after rebase, but got: {}",
        merge_note_after.unwrap_or_default()
    );
}

/// Same scenario but using `git pull --rebase`.
#[test]
fn test_pull_rebase_onto_branch_with_merge_commits_does_not_note_merge_commits() {
    let (local, _upstream) = TestRepo::new_with_remote();

    let mut base_file = local.filename("base.txt");
    base_file.set_contents(vec!["base line 1".human()]);
    let initial = local
        .stage_all_and_commit("initial commit")
        .expect("initial commit");
    local
        .git(&["push", "-u", "origin", "HEAD"])
        .expect("push initial");

    let branch = local.current_branch();

    // Create merge commit on upstream
    local
        .git(&["checkout", "-b", "side-branch"])
        .expect("create side branch");
    let mut side_file = local.filename("side.txt");
    side_file.set_contents(vec!["side content".human()]);
    local
        .stage_all_and_commit("side branch commit")
        .expect("side commit");

    local.git(&["checkout", &branch]).expect("switch to main");
    let mut main_file = local.filename("main_extra.txt");
    main_file.set_contents(vec!["main extra".human()]);
    local
        .stage_all_and_commit("main pre-merge commit")
        .expect("main pre-merge commit");

    local
        .git(&["merge", "--no-ff", "side-branch", "-m", "Merge side-branch"])
        .expect("merge");

    let merge_commit_sha = local
        .git(&["rev-parse", "HEAD"])
        .expect("merge sha")
        .trim()
        .to_string();

    local
        .git(&["push", "origin", &format!("HEAD:{}", branch)])
        .expect("push merge");

    // Reset local to before the merge and create a divergent AI commit
    local
        .git(&["reset", "--hard", &initial.commit_sha])
        .expect("reset to initial");

    let mut ai_file = local.filename("ai_feature.txt");
    ai_file.set_contents(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);
    local
        .stage_all_and_commit("add AI feature")
        .expect("AI commit");

    local.git(&["pull", "--rebase"]).expect("pull --rebase");

    // STRICT BLAME after pull --rebase
    ai_file.assert_lines_and_blame(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);
    base_file.assert_lines_and_blame(vec!["base line 1".human()]);

    let merge_note = local.read_authorship_note(&merge_commit_sha);
    assert!(
        merge_note.is_none(),
        "Merge commit should NOT have an authorship note after pull --rebase, but got: {}",
        merge_note.unwrap_or_default()
    );
}

/// Simulates the daemon fallback path where onto_head == merge_base.
/// Calls build_rebase_commit_mappings directly with Some(merge_base) to verify
/// merge commits on the target branch are excluded from new_commits.
/// Strict per-line blame assertions on all files.
#[test]
fn test_rebase_with_onto_equals_merge_base_does_not_note_merge_commits() {
    let repo = TestRepo::new();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(vec!["base line 1".human(), "base line 2".human()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit");

    let default_branch = repo.current_branch();

    // Create a merge commit on main via side branch
    repo.git(&["checkout", "-b", "side-branch"])
        .expect("create side branch");
    let mut side_file = repo.filename("side.txt");
    side_file.set_contents(vec!["side content".human()]);
    repo.stage_all_and_commit("side branch commit")
        .expect("side branch commit");

    repo.git(&["checkout", &default_branch])
        .expect("switch back to main");
    let mut main_file = repo.filename("main_extra.txt");
    main_file.set_contents(vec!["main extra content".human()]);
    repo.stage_all_and_commit("main commit before merge")
        .expect("main commit before merge");

    repo.git(&[
        "merge",
        "--no-ff",
        "side-branch",
        "-m",
        "Merge side-branch into main",
    ])
    .expect("merge side-branch");

    let merge_commit_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("merge sha")
        .trim()
        .to_string();

    let pre_merge_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .expect("pre-merge sha")
        .trim()
        .to_string();

    repo.git(&["checkout", "-b", "feature-daemon-sim", &pre_merge_sha])
        .expect("create feature branch");

    let mut ai_file = repo.filename("ai_daemon_sim.txt");
    ai_file.set_contents(vec![
        "AI daemon line 1".ai(),
        "AI daemon line 2".ai(),
        "AI daemon line 3".ai(),
    ]);
    repo.stage_all_and_commit("add AI feature for daemon sim")
        .expect("AI feature commit");

    let feature_commit_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("feature sha")
        .trim()
        .to_string();

    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed");

    let new_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("new head")
        .trim()
        .to_string();

    // Simulate daemon fallback: onto_head = merge_base(original_head, new_head)
    let merge_base_sha = repo
        .git(&["merge-base", &feature_commit_sha, &new_head])
        .expect("merge-base")
        .trim()
        .to_string();

    let path_str = repo.path().to_str().expect("valid path");
    let gitai_repo = git_ai::git::repository::find_repository_in_path(path_str).expect("open");
    let (original_commits, new_commits) =
        git_ai::commands::hooks::rebase_hooks::build_rebase_commit_mappings(
            &gitai_repo,
            &feature_commit_sha,
            &new_head,
            Some(&merge_base_sha),
        )
        .expect("build mappings");

    assert!(
        !new_commits.contains(&merge_commit_sha),
        "new_commits must not contain merge commit {} when onto_head == merge_base, got: {:?}",
        merge_commit_sha,
        new_commits
    );
    assert_eq!(
        original_commits.len(),
        1,
        "Should have 1 original commit, got: {:?}",
        original_commits
    );
    assert_eq!(
        new_commits.len(),
        1,
        "Should have 1 new commit, got: {:?}",
        new_commits
    );

    // STRICT LINE-LEVEL BLAME
    ai_file.assert_lines_and_blame(vec![
        "AI daemon line 1".ai(),
        "AI daemon line 2".ai(),
        "AI daemon line 3".ai(),
    ]);
    base_file.assert_lines_and_blame(vec!["base line 1".human(), "base line 2".human()]);
    main_file.assert_lines_and_blame(vec!["main extra content".human()]);
    side_file.assert_lines_and_blame(vec!["side content".human()]);

    assert!(
        repo.read_authorship_note(&merge_commit_sha).is_none(),
        "Merge commit must not have note after daemon-sim rebase, but got: {}",
        repo.read_authorship_note(&merge_commit_sha)
            .unwrap_or_default()
    );
}

/// Multiple AI feature commits rebased onto branch with merge commits.
/// Daemon fallback path (onto_head == merge_base). Mixed AI + human files.
/// Strict per-line blame assertions on every file and every line.
#[test]
fn test_rebase_multi_commit_with_onto_equals_merge_base_preserves_all_blame() {
    let repo = TestRepo::new();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(vec!["base content".human()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit");

    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "side-branch"])
        .expect("create side branch");
    let mut side_file = repo.filename("side.txt");
    side_file.set_contents(vec!["side line 1".human(), "side line 2".human()]);
    repo.stage_all_and_commit("side branch commit")
        .expect("side branch commit");

    repo.git(&["checkout", &default_branch])
        .expect("switch to main");
    let mut main_file = repo.filename("main_update.txt");
    main_file.set_contents(vec!["main update".human()]);
    repo.stage_all_and_commit("main commit")
        .expect("main commit");

    repo.git(&["merge", "--no-ff", "side-branch", "-m", "Merge side-branch"])
        .expect("merge");

    let merge_commit_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("merge sha")
        .trim()
        .to_string();

    let pre_merge_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .expect("pre-merge sha")
        .trim()
        .to_string();

    repo.git(&["checkout", "-b", "multi-feature", &pre_merge_sha])
        .expect("feature branch");

    // Commit 1: pure AI file
    let mut ai_file1 = repo.filename("ai_feat1.txt");
    ai_file1.set_contents(vec!["AI feature 1 line 1".ai(), "AI feature 1 line 2".ai()]);
    repo.stage_all_and_commit("AI feature 1")
        .expect("AI commit 1");

    // Commit 2: mixed AI + human
    let mut mixed_file = repo.filename("mixed.txt");
    mixed_file.set_contents(vec![
        "human context line".human(),
        "AI generated code".ai(),
        "another human line".human(),
    ]);
    repo.stage_all_and_commit("mixed commit")
        .expect("mixed commit");

    // Commit 3: pure AI file
    let mut ai_file2 = repo.filename("ai_feat2.txt");
    ai_file2.set_contents(vec!["AI feature 2 only line".ai()]);
    repo.stage_all_and_commit("AI feature 2")
        .expect("AI commit 2");

    let original_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("original head")
        .trim()
        .to_string();

    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed");

    let new_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("new head")
        .trim()
        .to_string();

    // Daemon fallback: onto = merge_base
    let merge_base_sha = repo
        .git(&["merge-base", &original_head, &new_head])
        .expect("merge-base")
        .trim()
        .to_string();

    let path_str = repo.path().to_str().expect("valid path");
    let gitai_repo = git_ai::git::repository::find_repository_in_path(path_str).expect("open");
    let (original_commits, new_commits) =
        git_ai::commands::hooks::rebase_hooks::build_rebase_commit_mappings(
            &gitai_repo,
            &original_head,
            &new_head,
            Some(&merge_base_sha),
        )
        .expect("build mappings");

    assert!(
        !new_commits.contains(&merge_commit_sha),
        "Merge commit {} must not be in new_commits, got: {:?}",
        merge_commit_sha,
        new_commits
    );
    assert_eq!(
        original_commits.len(),
        3,
        "Should have 3 original commits, got: {:?}",
        original_commits
    );
    assert_eq!(
        new_commits.len(),
        3,
        "Should have 3 new commits, got: {:?}",
        new_commits
    );

    // STRICT LINE-LEVEL BLAME on every file, every line

    ai_file1.assert_lines_and_blame(vec!["AI feature 1 line 1".ai(), "AI feature 1 line 2".ai()]);

    mixed_file.assert_lines_and_blame(vec![
        "human context line".human(),
        "AI generated code".ai(),
        "another human line".human(),
    ]);

    ai_file2.assert_lines_and_blame(vec!["AI feature 2 only line".ai()]);

    base_file.assert_lines_and_blame(vec!["base content".human()]);
    main_file.assert_lines_and_blame(vec!["main update".human()]);
    side_file.assert_lines_and_blame(vec!["side line 1".human(), "side line 2".human()]);

    assert!(
        repo.read_authorship_note(&merge_commit_sha).is_none(),
        "Merge commit must not have authorship note"
    );
}

/// Edge case: rebase onto a branch with MULTIPLE merge commits (busy main with
/// several merged PRs). Daemon fallback path (onto_head == merge_base).
#[test]
fn test_rebase_onto_multiple_merge_commits_with_onto_equals_merge_base() {
    let repo = TestRepo::new();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(vec!["base".human()]);
    repo.stage_all_and_commit("initial").expect("initial");

    let default_branch = repo.current_branch();
    let diverge_point = repo
        .git(&["rev-parse", "HEAD"])
        .expect("diverge")
        .trim()
        .to_string();

    // First merge commit on main
    repo.git(&["checkout", "-b", "pr-1"]).expect("create pr-1");
    let mut pr1_file = repo.filename("pr1.txt");
    pr1_file.set_contents(vec!["pr1 content".human()]);
    repo.stage_all_and_commit("pr-1 commit").expect("pr-1");

    repo.git(&["checkout", &default_branch])
        .expect("back to main");
    repo.git(&["merge", "--no-ff", "pr-1", "-m", "Merge PR #1"])
        .expect("merge pr-1");

    let merge1_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("merge1 sha")
        .trim()
        .to_string();

    // Second merge commit on main
    repo.git(&["checkout", "-b", "pr-2"]).expect("create pr-2");
    let mut pr2_file = repo.filename("pr2.txt");
    pr2_file.set_contents(vec!["pr2 content".human()]);
    repo.stage_all_and_commit("pr-2 commit").expect("pr-2");

    repo.git(&["checkout", &default_branch])
        .expect("back to main");
    repo.git(&["merge", "--no-ff", "pr-2", "-m", "Merge PR #2"])
        .expect("merge pr-2");

    let merge2_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("merge2 sha")
        .trim()
        .to_string();

    // Feature branch from diverge point
    repo.git(&["checkout", "-b", "my-feature", &diverge_point])
        .expect("feature branch");

    let mut ai_file = repo.filename("my_ai.txt");
    ai_file.set_contents(vec!["AI line alpha".ai(), "AI line beta".ai()]);
    repo.stage_all_and_commit("AI feature").expect("AI commit");

    let original_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("orig head")
        .trim()
        .to_string();

    repo.git(&["rebase", &default_branch]).expect("rebase");

    let new_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("new head")
        .trim()
        .to_string();

    // Daemon fallback
    let merge_base_sha = repo
        .git(&["merge-base", &original_head, &new_head])
        .expect("merge-base")
        .trim()
        .to_string();

    let path_str = repo.path().to_str().expect("valid path");
    let gitai_repo = git_ai::git::repository::find_repository_in_path(path_str).expect("open");
    let (original_commits, new_commits) =
        git_ai::commands::hooks::rebase_hooks::build_rebase_commit_mappings(
            &gitai_repo,
            &original_head,
            &new_head,
            Some(&merge_base_sha),
        )
        .expect("build mappings");

    assert!(
        !new_commits.contains(&merge1_sha),
        "new_commits must not contain merge PR #1 ({}), got: {:?}",
        merge1_sha,
        new_commits
    );
    assert!(
        !new_commits.contains(&merge2_sha),
        "new_commits must not contain merge PR #2 ({}), got: {:?}",
        merge2_sha,
        new_commits
    );
    assert_eq!(original_commits.len(), 1);
    assert_eq!(new_commits.len(), 1);

    // STRICT LINE-LEVEL BLAME
    ai_file.assert_lines_and_blame(vec!["AI line alpha".ai(), "AI line beta".ai()]);
    base_file.assert_lines_and_blame(vec!["base".human()]);
    pr1_file.assert_lines_and_blame(vec!["pr1 content".human()]);
    pr2_file.assert_lines_and_blame(vec!["pr2 content".human()]);

    assert!(
        repo.read_authorship_note(&merge1_sha).is_none(),
        "Merge PR #1 must not have a note"
    );
    assert!(
        repo.read_authorship_note(&merge2_sha).is_none(),
        "Merge PR #2 must not have a note"
    );
}

crate::reuse_tests_in_worktree!(
    test_rebase_onto_branch_with_merge_commits_does_not_note_merge_commits,
    test_pull_rebase_onto_branch_with_merge_commits_does_not_note_merge_commits,
    test_rebase_with_onto_equals_merge_base_does_not_note_merge_commits,
    test_rebase_multi_commit_with_onto_equals_merge_base_preserves_all_blame,
    test_rebase_onto_multiple_merge_commits_with_onto_equals_merge_base,
);
