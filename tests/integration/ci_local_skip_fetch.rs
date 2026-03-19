use crate::repos::test_repo::TestRepo;
use std::fs;

fn setup_squash_merge_with_local_only_base(repo: &TestRepo) -> (String, String, String, String) {
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.git_og(&["add", "file.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "base"]).unwrap();
    repo.git_og(&["push", "-u", "origin", "HEAD:main"]).unwrap();

    repo.git_og(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "base\nfeature line\n").unwrap();
    repo.git_og(&["commit", "-am", "feature change"]).unwrap();
    let feature_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    repo.git_og(&["push", "-u", "origin", "feature"]).unwrap();

    repo.git_og(&["checkout", "main"]).unwrap();
    let base_ref = "base/gh-reg-local".to_string();
    repo.git_og(&["checkout", "-b", &base_ref]).unwrap();
    let base_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    repo.git_og(&["checkout", "main"]).unwrap();
    repo.git_og(&["merge", "--squash", "feature"]).unwrap();
    repo.git_og(&["commit", "-m", "squash merge"]).unwrap();
    let merge_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    (merge_sha, feature_sha, base_sha, base_ref)
}

#[test]
fn test_ci_local_merge_fails_when_base_ref_only_exists_locally() {
    let (repo, _upstream) = TestRepo::new_with_remote();
    let (merge_sha, head_sha, base_sha, base_ref) = setup_squash_merge_with_local_only_base(&repo);

    let args = vec![
        "ci",
        "local",
        "merge",
        "--merge-commit-sha",
        merge_sha.as_str(),
        "--base-ref",
        base_ref.as_str(),
        "--head-ref",
        "feature",
        "--head-sha",
        head_sha.as_str(),
        "--base-sha",
        base_sha.as_str(),
    ];

    let err = repo
        .git_ai(&args)
        .expect_err("expected local ci to fail when base ref is not on origin");

    assert!(err.contains("Failed to fetch base branch"));
    assert!(err.contains(&base_ref));
}

#[test]
fn test_ci_local_merge_skip_fetch_base_allows_local_only_base_ref() {
    let (repo, _upstream) = TestRepo::new_with_remote();
    let (merge_sha, head_sha, base_sha, base_ref) = setup_squash_merge_with_local_only_base(&repo);

    let args = vec![
        "ci",
        "local",
        "merge",
        "--merge-commit-sha",
        merge_sha.as_str(),
        "--base-ref",
        base_ref.as_str(),
        "--head-ref",
        "feature",
        "--head-sha",
        head_sha.as_str(),
        "--base-sha",
        base_sha.as_str(),
        "--skip-fetch-base",
    ];

    let output = repo
        .git_ai(&args)
        .expect("expected local ci to succeed when --skip-fetch-base is set");

    assert!(output.contains("Skipping base branch fetch for"));
    assert!(output.contains("Local CI (merge): no AI authorship to track"));
}

#[test]
fn test_ci_local_merge_skip_fetch_notes_works_without_origin_remote() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.git_og(&["add", "file.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "base"]).unwrap();
    let sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let err_args = vec![
        "ci",
        "local",
        "merge",
        "--merge-commit-sha",
        sha.as_str(),
        "--base-ref",
        "main",
        "--head-ref",
        "feature",
        "--head-sha",
        sha.as_str(),
        "--base-sha",
        sha.as_str(),
    ];
    let err = repo
        .git_ai(&err_args)
        .expect_err("expected local ci to fail without origin and without skip flag");
    assert!(err.contains("Error running local CI"));

    let ok_args = vec![
        "ci",
        "local",
        "merge",
        "--merge-commit-sha",
        sha.as_str(),
        "--base-ref",
        "main",
        "--head-ref",
        "feature",
        "--head-sha",
        sha.as_str(),
        "--base-sha",
        sha.as_str(),
        "--skip-fetch",
    ];
    let output = repo
        .git_ai(&ok_args)
        .expect("expected local ci to succeed with --skip-fetch");

    assert!(output.contains("Skipping authorship history fetch"));
    assert!(output.contains("Local CI (merge): skipped fast-forward merge"));
}

#[test]
fn test_ci_local_merge_skip_fetch_base_fails_if_base_ref_missing_locally() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.git_og(&["add", "file.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "base"]).unwrap();
    let first_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    fs::write(&file_path, "base\nnext\n").unwrap();
    repo.git_og(&["commit", "-am", "next"]).unwrap();
    let second_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let args = vec![
        "ci",
        "local",
        "merge",
        "--merge-commit-sha",
        second_sha.as_str(),
        "--base-ref",
        "base/does-not-exist",
        "--head-ref",
        "feature",
        "--head-sha",
        first_sha.as_str(),
        "--base-sha",
        first_sha.as_str(),
        "--skip-fetch-notes",
        "--skip-fetch-base",
    ];

    let err = repo
        .git_ai(&args)
        .expect_err("expected failure when --skip-fetch-base is set and base ref is missing");
    assert!(err.contains("Failed to resolve base ref 'base/does-not-exist' locally"));
}

crate::reuse_tests_in_worktree!(
    test_ci_local_merge_fails_when_base_ref_only_exists_locally,
    test_ci_local_merge_skip_fetch_base_allows_local_only_base_ref,
    test_ci_local_merge_skip_fetch_notes_works_without_origin_remote,
    test_ci_local_merge_skip_fetch_base_fails_if_base_ref_missing_locally,
);
