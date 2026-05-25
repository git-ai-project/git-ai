use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

crate::reuse_tests_in_worktree!(test_ci_local_merge_skip_push_leaves_remote_notes_untouched,);

#[test]
fn test_ci_local_merge_skip_push_leaves_remote_notes_untouched() {
    let (repo, upstream) = TestRepo::new_with_remote();

    // base commit on main, pushed to origin
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    let base_sha = repo.stage_all_and_commit("base").unwrap().commit_sha;
    repo.git_og(&["branch", "-M", "main"]).unwrap();
    repo.git_og(&["push", "-u", "origin", "main"]).unwrap();

    // feature branch with an AI-attributed commit
    repo.git_og(&["checkout", "-b", "feature"]).unwrap();
    let mut feat = repo.filename("feat.txt");
    feat.set_contents(crate::lines!["// ai line".ai()]);
    let head_sha = repo.stage_all_and_commit("feat").unwrap().commit_sha;
    repo.git_og(&["push", "-u", "origin", "feature"]).unwrap();

    // simulate squash merge on main (new commit, no note yet)
    repo.git_og(&["checkout", "main"]).unwrap();
    repo.git_og(&["merge", "--squash", "feature"]).unwrap();
    repo.git_og(&["commit", "-m", "squash merge"]).unwrap();
    let merge_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    repo.git_og(&["push", "origin", "main"]).unwrap();

    let remote_notes_before = upstream
        .git_og(&["rev-parse", "--verify", "refs/notes/ai"])
        .ok()
        .map(|s| s.trim().to_string());

    let output = repo
        .git_ai(&[
            "ci",
            "local",
            "merge",
            "--merge-commit-sha",
            merge_sha.as_str(),
            "--base-ref",
            "main",
            "--head-ref",
            "feature",
            "--head-sha",
            head_sha.as_str(),
            "--base-sha",
            base_sha.as_str(),
            "--skip-fetch",
            "--skip-push",
        ])
        .expect("expected local ci to succeed with --skip-push");

    let remote_notes_after = upstream
        .git_og(&["rev-parse", "--verify", "refs/notes/ai"])
        .ok()
        .map(|s| s.trim().to_string());

    // the rewrite branch executed and push was suppressed
    assert!(
        output.contains("Skipping authorship push"),
        "expected --skip-push log line, got: {}",
        output
    );
    assert!(
        !output.contains("Pushing authorship..."),
        "expected no push attempt, got: {}",
        output
    );

    // remote must be untouched
    assert_eq!(
        remote_notes_before, remote_notes_after,
        "remote refs/notes/ai must be unchanged with --skip-push"
    );
}
