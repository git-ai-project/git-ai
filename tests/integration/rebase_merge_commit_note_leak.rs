use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// Guard test: after rebasing onto a branch with merge commits, the merge commits
/// on the target branch must NOT receive AI authorship notes.
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

    ai_file.assert_lines_and_blame(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);
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

    ai_file.assert_lines_and_blame(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);
    base_file.assert_lines_and_blame(vec!["base line 1".human()]);

    let merge_note = local.read_authorship_note(&merge_commit_sha);
    assert!(
        merge_note.is_none(),
        "Merge commit should NOT have an authorship note after pull --rebase, but got: {}",
        merge_note.unwrap_or_default()
    );
}

crate::reuse_tests_in_worktree!(
    test_rebase_onto_branch_with_merge_commits_does_not_note_merge_commits,
    test_pull_rebase_onto_branch_with_merge_commits_does_not_note_merge_commits,
);
