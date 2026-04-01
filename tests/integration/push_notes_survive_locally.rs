use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// Regression test for: authorship notes disappear from the local repo after push.
///
/// Reproduces the exact workflow from the bug report:
/// 1. Commit with AI content → notes present locally  ✓
/// 2. Commit again with AI content → notes present locally  ✓
/// 3. Push → notes gone locally  ✗  (bug)
///
/// The existing push_upstream_authorship tests only verify that notes arrive on the
/// remote. This test verifies they also **survive locally** after the push.
#[test]
fn push_does_not_delete_local_authorship_notes() {
    let (local, upstream) = TestRepo::new_with_remote();

    // --- First commit with AI content ---
    let mut file = local.filename("feature.rs");
    file.set_contents(vec!["fn feature_one() {}".ai()]);
    let commit1 = local
        .stage_all_and_commit("first AI commit")
        .expect("first commit should succeed");

    // Notes must exist locally right after commit
    let note1_before_push = local.read_authorship_note(&commit1.commit_sha);
    assert!(
        note1_before_push.is_some(),
        "expected authorship note on first commit immediately after committing"
    );

    // --- Second commit with AI content ---
    file.set_contents(vec![
        "fn feature_one() {}".ai(),
        "fn feature_two() {}".ai(),
    ]);
    let commit2 = local
        .stage_all_and_commit("second AI commit")
        .expect("second commit should succeed");

    // Notes must exist locally for both commits before push
    let note2_before_push = local.read_authorship_note(&commit2.commit_sha);
    assert!(
        note2_before_push.is_some(),
        "expected authorship note on second commit before push"
    );
    let note1_still = local.read_authorship_note(&commit1.commit_sha);
    assert!(
        note1_still.is_some(),
        "expected authorship note on first commit to still exist before push"
    );

    // --- Push to remote ---
    local
        .git(&["push", "-u", "origin", "HEAD"])
        .expect("push should succeed");

    // --- Verify notes survived locally after push ---
    let note1_after_push = local.read_authorship_note(&commit1.commit_sha);
    assert!(
        note1_after_push.is_some(),
        "BUG: authorship note on first commit disappeared after push"
    );

    let note2_after_push = local.read_authorship_note(&commit2.commit_sha);
    assert!(
        note2_after_push.is_some(),
        "BUG: authorship note on second commit disappeared after push"
    );

    // Notes content should be unchanged
    assert_eq!(
        note1_before_push.unwrap(),
        note1_after_push.unwrap(),
        "authorship note content on first commit changed after push"
    );
    assert_eq!(
        note2_before_push.unwrap(),
        note2_after_push.unwrap(),
        "authorship note content on second commit changed after push"
    );

    // Also verify notes made it to the remote (existing behavior)
    let remote_note1 =
        local.read_authorship_note_in_git_dir(upstream.path(), &commit1.commit_sha);
    assert!(
        remote_note1.is_some(),
        "expected authorship note on first commit to exist on remote"
    );

    let remote_note2 =
        local.read_authorship_note_in_git_dir(upstream.path(), &commit2.commit_sha);
    assert!(
        remote_note2.is_some(),
        "expected authorship note on second commit to exist on remote"
    );
}

/// Same scenario but using `git push` (no -u flag) after upstream is already configured.
/// This matches workflows where the branch was previously pushed with -u.
#[test]
fn push_without_u_flag_preserves_local_authorship_notes() {
    let (local, upstream) = TestRepo::new_with_remote();

    // Bootstrap: push initial commit to set up tracking
    let mut file = local.filename("service.rs");
    file.set_contents(vec!["fn bootstrap() {}".ai()]);
    local
        .stage_all_and_commit("bootstrap")
        .expect("bootstrap commit should succeed");
    local
        .git(&["push", "-u", "origin", "HEAD"])
        .expect("initial push should succeed");

    // --- First real commit ---
    file.set_contents(vec![
        "fn bootstrap() {}".ai(),
        "fn handler() {}".ai(),
    ]);
    let commit1 = local
        .stage_all_and_commit("add handler")
        .expect("commit should succeed");

    let note1_before = local.read_authorship_note(&commit1.commit_sha);
    assert!(
        note1_before.is_some(),
        "expected note on commit before push"
    );

    // --- Second commit ---
    file.set_contents(vec![
        "fn bootstrap() {}".ai(),
        "fn handler() {}".ai(),
        "fn middleware() {}".ai(),
    ]);
    let commit2 = local
        .stage_all_and_commit("add middleware")
        .expect("commit should succeed");

    let note2_before = local.read_authorship_note(&commit2.commit_sha);
    assert!(
        note2_before.is_some(),
        "expected note on second commit before push"
    );

    // --- Push without -u (upstream already set) ---
    local
        .git(&["push"])
        .expect("push should succeed");

    // --- Verify local notes survive ---
    let note1_after = local.read_authorship_note(&commit1.commit_sha);
    assert!(
        note1_after.is_some(),
        "BUG: authorship note on first commit disappeared after push (no -u)"
    );

    let note2_after = local.read_authorship_note(&commit2.commit_sha);
    assert!(
        note2_after.is_some(),
        "BUG: authorship note on second commit disappeared after push (no -u)"
    );

    // Content unchanged
    assert_eq!(
        note1_before.unwrap(),
        note1_after.unwrap(),
        "note content changed after push"
    );
    assert_eq!(
        note2_before.unwrap(),
        note2_after.unwrap(),
        "note content changed after push"
    );

    // Remote has them too
    let remote_note = local.read_authorship_note_in_git_dir(upstream.path(), &commit2.commit_sha);
    assert!(
        remote_note.is_some(),
        "expected note on remote after push"
    );
}

crate::reuse_tests_in_worktree!(
    push_does_not_delete_local_authorship_notes,
    push_without_u_flag_preserves_local_authorship_notes,
);
