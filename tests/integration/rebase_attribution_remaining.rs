use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

// =============================================================================
// ISSUE-003: pull --rebase --autostash FF drops uncommitted AI attribution
// =============================================================================

/// When the local branch is strictly behind the remote (fast-forward case) and
/// the developer has uncommitted AI-authored changes, `git pull --rebase --autostash`
/// should preserve the AI attribution after the stash cycle.
#[test]
fn test_pull_rebase_autostash_ff_preserves_uncommitted_ai_attribution() {
    let (local, _upstream) = TestRepo::new_with_remote();

    // Initial commit: push to remote so both sides share a base
    let mut base_file = local.filename("base.txt");
    base_file.set_contents(crate::lines!["base content".human()]);
    local.stage_all_and_commit("initial").unwrap();
    local.git(&["push", "-u", "origin", "HEAD"]).unwrap();

    // Advance remote: create a second commit and push it
    let mut remote_file = local.filename("remote.txt");
    remote_file.set_contents(crate::lines!["remote content".human()]);
    local.stage_all_and_commit("remote advance").unwrap();
    local.git(&["push", "origin", "HEAD"]).unwrap();

    // Reset local back to initial (so local is behind by 1 commit = FF scenario)
    let initial_sha = local
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    local.git(&["reset", "--hard", &initial_sha]).unwrap();

    // Local: AI edits base.txt without committing
    base_file.set_contents(crate::lines![
        "base content".human(),
        "ai line 1".ai(),
        "ai line 2".ai()
    ]);
    local.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    // Pull with autostash (FF case -- no diverged local commits)
    local
        .git(&["pull", "--rebase", "--autostash", "origin", "HEAD"])
        .unwrap();

    // Commit the restored changes
    local
        .stage_all_and_commit("ai work after autostash ff")
        .unwrap();

    // Attribution must be preserved
    base_file.assert_lines_and_blame(crate::lines![
        "base content".human(),
        "ai line 1".ai(),
        "ai line 2".ai()
    ]);
}

// =============================================================================
// ISSUE-007: git rebase --no-verify drops attribution
// =============================================================================

/// `git rebase --no-verify` should still transfer AI attribution notes from
/// original commit SHAs to the new rebased commit SHAs.
#[test]
fn test_rebase_no_verify_preserves_attribution() {
    let repo = TestRepo::new();

    // Initial commit on default branch
    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["v1".human()]);
    repo.stage_all_and_commit("initial").unwrap();
    let main_branch = repo.current_branch();

    // Feature branch: AI commit
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feat = repo.filename("feat.txt");
    feat.set_contents(crate::lines!["ai feature line".ai()]);
    repo.stage_all_and_commit("add feature").unwrap();

    // Advance main
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut other = repo.filename("other.txt");
    other.set_contents(crate::lines!["other".human()]);
    repo.stage_all_and_commit("main advances").unwrap();

    // Rebase with --no-verify
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "--no-verify", &main_branch]).unwrap();

    // Verify authorship was preserved after --no-verify rebase
    feat.assert_lines_and_blame(crate::lines!["ai feature line".ai()]);
}

// =============================================================================
// ISSUE-011: git rebase -i --autosquash (fixup!) loses attribution
// =============================================================================

/// When `git rebase -i --autosquash` squashes a fixup! commit into its target,
/// the resulting single commit should preserve AI attribution from both sources.
#[test]
fn test_autosquash_preserves_combined_ai_attribution() {
    let repo = TestRepo::new();

    // Initial commit (needed as the rebase base)
    let mut dummy = repo.filename("dummy.txt");
    dummy.set_contents(crate::lines!["init".human()]);
    repo.stage_all_and_commit("init").unwrap();

    // Base AI commit
    let mut f = repo.filename("validator.py");
    f.set_contents(crate::lines![
        "def validate_email(): pass".ai(),
        "def validate_url(): pass".ai()
    ]);
    repo.stage_all_and_commit("add validators").unwrap();

    // Fixup commit: add more AI content
    f.set_contents(crate::lines![
        "def validate_email(): pass".ai(),
        "def validate_url(): pass".ai(),
        "def validate_phone(): pass".ai()
    ]);
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "fixup! add validators"])
        .unwrap();

    // Autosquash: squash the fixup into the base commit
    repo.git_with_env(
        &["rebase", "-i", "--autosquash", "HEAD~2"],
        &[("GIT_SEQUENCE_EDITOR", "true")],
        None,
    )
    .unwrap();

    // The squashed commit should have AI attribution for all lines
    f.assert_lines_and_blame(crate::lines![
        "def validate_email(): pass".ai(),
        "def validate_url(): pass".ai(),
        "def validate_phone(): pass".ai()
    ]);
}

// =============================================================================
// ISSUE-014: git rebase -i with edit + commit --amend creates commits with no notes
// =============================================================================

/// When using `git rebase -i` with `edit` and then `commit --amend`, the final
/// commit should preserve AI attribution notes.
#[test]
fn test_interactive_rebase_edit_amend_preserves_notes() {
    let repo = TestRepo::new();

    // Initial commit
    let mut dummy = repo.filename("dummy.txt");
    dummy.set_contents(crate::lines!["init".human()]);
    repo.stage_all_and_commit("init").unwrap();

    // AI commit
    let mut f = repo.filename("module.py");
    f.set_contents(crate::lines![
        "def foo(): pass".ai(),
        "def bar(): pass".ai()
    ]);
    repo.stage_all_and_commit("add module").unwrap();

    let original_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Verify AI attribution exists before rebase
    let pre_note = repo.read_authorship_note(&original_sha);
    assert!(
        pre_note.is_some(),
        "AI commit should have authorship note before rebase"
    );

    // Interactive rebase: mark the commit as 'edit' then amend.
    // Use perl -i -pe for cross-platform in-place editing (macOS BSD sed
    // requires a backup suffix with -i, unlike GNU sed).
    repo.git_with_env(
        &["rebase", "-i", "HEAD~1"],
        &[("GIT_SEQUENCE_EDITOR", "perl -i -pe 's/^pick/edit/'")],
        None,
    )
    .unwrap();

    // At the edit stop: add a comment and amend
    f.set_contents(crate::lines![
        "# amended".human(),
        "def foo(): pass".ai(),
        "def bar(): pass".ai()
    ]);
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "--no-edit"]).unwrap();
    repo.git(&["rebase", "--continue"]).unwrap();

    let new_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_ne!(original_sha, new_sha, "SHA should change after edit+amend");

    // Verify AI attribution survived
    f.assert_lines_and_blame(crate::lines![
        "# amended".human(),
        "def foo(): pass".ai(),
        "def bar(): pass".ai()
    ]);
}

crate::reuse_tests_in_worktree!(
    test_pull_rebase_autostash_ff_preserves_uncommitted_ai_attribution,
    test_rebase_no_verify_preserves_attribution,
    test_autosquash_preserves_combined_ai_attribution,
    test_interactive_rebase_edit_amend_preserves_notes,
);
