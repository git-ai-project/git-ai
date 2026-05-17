use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// Helper: creates a local+upstream pair with one AI-attributed commit already made,
/// returns (local, upstream, commit_sha).
fn setup_repo_with_ai_commit() -> (TestRepo, TestRepo, String) {
    let (local, upstream) = TestRepo::new_with_remote();

    let mut file = local.filename("push_test.rs");
    file.set_contents(vec!["fn ai_generated() {}".ai()]);
    let commit = local
        .stage_all_and_commit("add AI-attributed code")
        .expect("commit should succeed");

    (local, upstream, commit.commit_sha)
}

/// --dry-run should NOT push authorship notes because nothing actually gets pushed.
#[test]
#[ignore = "v2 branch does not yet detect --dry-run in push hooks"]
fn push_with_dry_run_does_not_push_authorship_notes() {
    let (local, upstream, commit_sha) = setup_repo_with_ai_commit();

    local
        .git(&["push", "--dry-run", "origin", "HEAD"])
        .expect("push --dry-run should succeed");

    let note = local.read_authorship_note_in_git_dir(upstream.path(), &commit_sha);
    assert!(
        note.is_none(),
        "expected authorship notes NOT to be pushed when using --dry-run"
    );
}

/// Force push should still push authorship notes (regression test).
#[test]
fn force_push_still_pushes_authorship_notes() {
    let (local, upstream, _first_sha) = setup_repo_with_ai_commit();

    // Initial push to establish the remote branch
    local
        .git(&["push", "-u", "origin", "HEAD"])
        .expect("initial push should succeed");

    // Create a new commit for force-push scenario
    let mut file = local.filename("push_test_force.rs");
    file.set_contents(vec!["fn force_pushed() {}".ai()]);
    let force_commit = local
        .stage_all_and_commit("force push commit")
        .expect("new commit should succeed");

    // Force push -- this should still trigger authorship note push
    local
        .git(&["push", "--force", "origin", "HEAD"])
        .expect("force push should succeed");

    let note = local.read_authorship_note_in_git_dir(upstream.path(), &force_commit.commit_sha);
    assert!(
        note.is_some(),
        "expected authorship notes to be pushed even when using --force"
    );
}

/// Pushing to an explicitly named remote should push notes to that remote.
#[test]
fn push_to_explicit_remote_name_pushes_notes_to_that_remote() {
    let (local, upstream) = TestRepo::new_with_remote();

    let mut file = local.filename("explicit_remote.rs");
    file.set_contents(vec!["fn explicit_remote_test() {}".ai()]);
    let commit = local
        .stage_all_and_commit("commit for explicit remote push")
        .expect("commit should succeed");

    // Push explicitly naming "origin" as the remote (without -u flag)
    local
        .git(&["push", "origin", "HEAD"])
        .expect("push to explicit remote should succeed");

    let note = local.read_authorship_note_in_git_dir(upstream.path(), &commit.commit_sha);
    assert!(
        note.is_some(),
        "expected authorship notes to be pushed when explicitly specifying remote name"
    );
}

/// Multiple pushes to same remote accumulate authorship notes for all commits.
#[test]
fn multiple_pushes_accumulate_authorship_notes() {
    let (local, upstream) = TestRepo::new_with_remote();

    // First commit and push
    let mut file1 = local.filename("multi1.rs");
    file1.set_contents(vec!["fn first() {}".ai()]);
    let commit1 = local
        .stage_all_and_commit("first commit")
        .expect("first commit should succeed");
    local
        .git(&["push", "-u", "origin", "HEAD"])
        .expect("first push should succeed");

    // Second commit and push
    let mut file2 = local.filename("multi2.rs");
    file2.set_contents(vec!["fn second() {}".ai()]);
    let commit2 = local
        .stage_all_and_commit("second commit")
        .expect("second commit should succeed");
    local
        .git(&["push", "origin", "HEAD"])
        .expect("second push should succeed");

    // Both commits should have notes on the remote
    let note1 = local.read_authorship_note_in_git_dir(upstream.path(), &commit1.commit_sha);
    let note2 = local.read_authorship_note_in_git_dir(upstream.path(), &commit2.commit_sha);
    assert!(
        note1.is_some(),
        "first commit should have authorship note on remote"
    );
    assert!(
        note2.is_some(),
        "second commit should have authorship note on remote"
    );
}

/// Push with no AI commits should still succeed (no authorship to push).
#[test]
fn push_with_no_ai_commits_succeeds() {
    let (local, _upstream) = TestRepo::new_with_remote();

    // Pure human commit (no checkpoint)
    let file_path = local.path().join("human_only.rs");
    std::fs::write(&file_path, "fn human_only() {}\n").unwrap();
    local.stage_all_and_commit("human commit").unwrap();

    // Push should succeed without errors even with no notes to push
    let result = local.git(&["push", "-u", "origin", "HEAD"]);
    assert!(
        result.is_ok(),
        "push should succeed even without authorship notes: {:?}",
        result
    );
}

crate::reuse_tests_in_worktree!(
    force_push_still_pushes_authorship_notes,
    push_to_explicit_remote_name_pushes_notes_to_that_remote,
    multiple_pushes_accumulate_authorship_notes,
    push_with_no_ai_commits_succeeds,
);
