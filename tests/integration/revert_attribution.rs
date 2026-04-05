use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// ISSUE-015: revert-of-revert should restore original AI attribution
#[test]
fn test_revert_of_revert_restores_ai_attribution() {
    let repo = TestRepo::new();

    // Create initial commit with a base file
    let mut file = repo.filename("module.py");
    file.set_contents(crate::lines!["base line"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Commit A: AI creates content
    file.insert_at(1, crate::lines!["ai line 1".ai(), "ai line 2".ai()]);
    let commit_a = repo.stage_all_and_commit("Add AI module").unwrap();

    // Verify commit A has AI attribution
    let stats_a = repo.stats().unwrap();
    assert!(
        stats_a.ai_additions > 0,
        "Commit A should have ai_additions > 0, got {}",
        stats_a.ai_additions
    );
    let ai_additions_a = stats_a.ai_additions;

    // Commit B: Revert commit A (deletes the AI content)
    repo.git(&["revert", "--no-edit", &commit_a.commit_sha])
        .unwrap();

    // Commit C: Revert the revert (restores the AI content)
    let revert_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    repo.git(&["revert", "--no-edit", &revert_sha]).unwrap();

    // Verify commit C has AI attribution matching commit A
    let stats_c = repo.stats().unwrap();
    assert!(
        stats_c.ai_additions > 0,
        "Commit C (revert-of-revert) should have ai_additions > 0, got {}. Expected {} (matching commit A)",
        stats_c.ai_additions,
        ai_additions_a
    );
}

/// ISSUE-004: reverting an AI commit (deletion) should show ai=0 (correct behavior to preserve)
#[test]
fn test_revert_of_ai_commit_is_attribution_neutral() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("module.py");
    file.set_contents(crate::lines!["base line"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates file content -> commit A (ai > 0)
    file.insert_at(1, crate::lines!["ai content".ai()]);
    let commit_a = repo.stage_all_and_commit("Add AI content").unwrap();

    let stats_a = repo.stats().unwrap();
    assert!(
        stats_a.ai_additions > 0,
        "Commit A should have ai_additions > 0"
    );

    // Revert commit A -> commit B (pure deletion, should have ai=0)
    repo.git(&["revert", "--no-edit", &commit_a.commit_sha])
        .unwrap();

    let stats_b = repo.stats().unwrap();
    assert_eq!(
        stats_b.ai_additions, 0,
        "Commit B (revert of AI commit) should have ai_additions=0 since it only deletes lines"
    );

    // Verify commit A's note is unchanged
    let note_a = repo
        .read_authorship_note(&commit_a.commit_sha)
        .expect("Commit A should still have its authorship note");
    assert!(
        !note_a.is_empty(),
        "Commit A's authorship note should not be empty"
    );
}

/// The original AI commit's note must not be modified by a revert
#[test]
fn test_revert_preserves_original_commit_note() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("module.py");
    file.set_contents(crate::lines!["base line"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates content
    file.insert_at(1, crate::lines!["ai feature".ai()]);
    let commit_a = repo.stage_all_and_commit("Add AI feature").unwrap();

    // Read commit A's note before revert
    let note_before = repo
        .read_authorship_note(&commit_a.commit_sha)
        .expect("Commit A should have authorship note");

    // Revert commit A
    repo.git(&["revert", "--no-edit", &commit_a.commit_sha])
        .unwrap();

    // Revert the revert
    let revert_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    repo.git(&["revert", "--no-edit", &revert_sha]).unwrap();

    // Read commit A's note after both reverts
    let note_after = repo
        .read_authorship_note(&commit_a.commit_sha)
        .expect("Commit A should still have authorship note after reverts");

    assert_eq!(
        note_before, note_after,
        "Commit A's authorship note should not be modified by reverts"
    );
}

crate::reuse_tests_in_worktree!(
    test_revert_of_revert_restores_ai_attribution,
    test_revert_of_ai_commit_is_attribution_neutral,
    test_revert_preserves_original_commit_note,
);
