use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;

#[test]
fn ci_squash_does_not_credit_unchanged_incoming_path() {
    let repo = TestRepo::new();
    let mut seed = repo.filename("base.txt");
    seed.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    seed.assert_committed_lines(crate::lines!["base".human()]);
    repo.git(&["branch", "-M", "main"]).unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut deleted = repo.filename("deleted.txt");
    let mut original = repo.filename("original.txt");
    deleted.set_contents(crate::lines!["shared contents".ai()]);
    original.set_contents(crate::lines!["retained feature work".ai()]);
    repo.stage_all_and_commit("Add AI feature files").unwrap();
    deleted.assert_committed_lines(crate::lines!["shared contents".ai()]);
    original.assert_committed_lines(crate::lines!["retained feature work".ai()]);

    repo.git(&["rm", "deleted.txt"]).unwrap();
    repo.git(&["mv", "original.txt", "renamed.txt"]).unwrap();
    repo.commit("Delete one file and rename the other").unwrap();
    let mut renamed = repo.filename("renamed.txt");
    renamed.assert_committed_lines(crate::lines!["retained feature work".ai()]);

    repo.git(&["checkout", "main"]).unwrap();
    let mut incoming = repo.filename("incoming.txt");
    incoming.set_contents(crate::lines!["shared contents"]);
    let base_sha = repo
        .stage_all_and_commit("Add base branch file")
        .unwrap()
        .commit_sha;
    incoming.assert_committed_lines(crate::lines!["shared contents".human()]);

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["merge", "--no-edit", "main"]).unwrap();
    renamed.assert_committed_lines(crate::lines!["retained feature work".ai()]);
    incoming.assert_committed_lines(crate::lines!["shared contents".human()]);
    let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap();

    // Make the squash via raw git so the CI command is the only note writer.
    repo.git_og(&["checkout", "main"]).unwrap();
    repo.git_og(&["merge", "--squash", "feature"]).unwrap();
    repo.git_og(&["commit", "-m", "Squash feature"]).unwrap();
    let merge_sha = repo.git_og(&["rev-parse", "HEAD"]).unwrap();
    let merge_sha = merge_sha.trim();
    let result = repo
        .git_ai(&[
            "ci",
            "local",
            "merge",
            "--merge-commit-sha",
            merge_sha,
            "--base-ref",
            "main",
            "--head-ref",
            "feature",
            "--head-sha",
            head_sha.trim(),
            "--base-sha",
            &base_sha,
            "--skip-fetch",
            "--skip-push",
        ])
        .unwrap();
    assert!(
        result.contains("authorship rewritten successfully"),
        "{result}"
    );

    renamed.assert_committed_lines(crate::lines!["retained feature work".ai()]);
    incoming.assert_committed_lines(crate::lines!["shared contents".human()]);
    let note = repo.read_authorship_note(merge_sha).expect("squash note");
    let log = AuthorshipLog::deserialize_from_string(&note).unwrap();
    let paths: Vec<&str> = log
        .attestations
        .iter()
        .map(|attestation| attestation.file_path.as_str())
        .collect();
    assert_eq!(paths, ["renamed.txt"]);
}
