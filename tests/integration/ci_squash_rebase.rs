use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;

fn run_ci_local_merge(repo: &TestRepo, merge_sha: &str, head_sha: &str, base_sha: &str) -> String {
    repo.git_ai(&[
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
        head_sha,
        "--base-sha",
        base_sha,
        "--skip-fetch",
        "--skip-push",
    ])
    .expect("ci local merge should succeed")
}

fn assert_ci_rewrite_succeeded(output: &str) {
    assert!(
        output.contains("authorship rewritten successfully"),
        "expected ci local merge to rewrite authorship, got: {output}"
    );
}

fn authorship_files(repo: &TestRepo, commit_sha: &str) -> Vec<String> {
    let note = repo
        .read_authorship_note(commit_sha)
        .unwrap_or_else(|| panic!("expected authorship note for {commit_sha}"));
    AuthorshipLog::deserialize_from_string(&note)
        .expect("authorship note should deserialize")
        .attestations
        .iter()
        .map(|attestation| attestation.file_path.clone())
        .collect()
}

fn setup_main(repo: &TestRepo) -> String {
    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["base"]);
    let base_sha = repo.stage_all_and_commit("base").unwrap().commit_sha;
    repo.git(&["branch", "-M", "main"]).unwrap();
    base_sha
}

fn squash_feature_with_raw_git(repo: &TestRepo, message: &str) -> String {
    repo.git_og(&["checkout", "main"]).unwrap();
    repo.git_og(&["merge", "--squash", "feature"]).unwrap();
    repo.git_og(&["commit", "-m", message]).unwrap();
    repo.git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn test_ci_squash_merge_basic() {
    let repo = TestRepo::new();
    let base_sha = setup_main(&repo);

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature = repo.filename("feature.js");
    feature.set_contents(crate::lines![
        "export function aiFeature() {".ai(),
        "  return 'ai code';".ai(),
        "}".ai()
    ]);
    let head_sha = repo
        .stage_all_and_commit("add ai feature")
        .unwrap()
        .commit_sha;

    let merge_sha = squash_feature_with_raw_git(&repo, "squash feature");
    let output = run_ci_local_merge(&repo, &merge_sha, &head_sha, &base_sha);
    assert_ci_rewrite_succeeded(&output);

    feature.assert_lines_and_blame(crate::lines![
        "export function aiFeature() {".ai(),
        "  return 'ai code';".ai(),
        "}".ai()
    ]);
}

#[test]
fn test_ci_squash_merge_multiple_files() {
    let repo = TestRepo::new();
    let base_sha = setup_main(&repo);

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut api = repo.filename("api.js");
    let mut view = repo.filename("view.js");
    api.set_contents(crate::lines![
        "export const handler = () => {".ai(),
        "  return 'ok';".ai(),
        "};".ai()
    ]);
    view.set_contents(crate::lines![
        "export function View() {".ai(),
        "  return handler();".ai(),
        "}".ai()
    ]);
    let head_sha = repo
        .stage_all_and_commit("add ai feature files")
        .unwrap()
        .commit_sha;

    let merge_sha = squash_feature_with_raw_git(&repo, "squash feature files");
    let output = run_ci_local_merge(&repo, &merge_sha, &head_sha, &base_sha);
    assert_ci_rewrite_succeeded(&output);

    api.assert_lines_and_blame(crate::lines![
        "export const handler = () => {".ai(),
        "  return 'ok';".ai(),
        "};".ai()
    ]);
    view.assert_lines_and_blame(crate::lines![
        "export function View() {".ai(),
        "  return handler();".ai(),
        "}".ai()
    ]);
}

#[test]
fn test_ci_squash_merge_mixed_ai_and_human_content() {
    let repo = TestRepo::new();
    let base_sha = setup_main(&repo);

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut mixed = repo.filename("mixed.js");
    mixed.set_contents(crate::lines![
        "// Human-written setup",
        "const flag = true;",
        "// AI generated helper".ai(),
        "function helper() {".ai(),
        "  return flag;".ai(),
        "}".ai(),
        "// Human-written footer"
    ]);
    let head_sha = repo
        .stage_all_and_commit("add mixed feature")
        .unwrap()
        .commit_sha;

    let merge_sha = squash_feature_with_raw_git(&repo, "squash mixed feature");
    let output = run_ci_local_merge(&repo, &merge_sha, &head_sha, &base_sha);
    assert_ci_rewrite_succeeded(&output);

    mixed.assert_lines_and_blame(crate::lines![
        "// Human-written setup".human(),
        "const flag = true;".human(),
        "// AI generated helper".ai(),
        "function helper() {".ai(),
        "  return flag;".ai(),
        "}".ai(),
        "// Human-written footer".human()
    ]);
}

#[test]
fn test_ci_squash_merge_no_notes_no_authorship_created() {
    let repo = TestRepo::new();

    let file_path = repo.path().join("feature.txt");
    std::fs::write(&file_path, "base\n").unwrap();
    repo.git_og(&["add", "feature.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "base"]).unwrap();
    let base_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    repo.git_og(&["branch", "-M", "main"]).unwrap();

    repo.git_og(&["checkout", "-b", "feature"]).unwrap();
    std::fs::write(&file_path, "base\nhuman change\n").unwrap();
    repo.git_og(&["commit", "-am", "human feature"]).unwrap();
    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let merge_sha = squash_feature_with_raw_git(&repo, "squash human feature");
    let output = run_ci_local_merge(&repo, &merge_sha, &head_sha, &base_sha);

    assert!(
        output.contains("no AI authorship to track"),
        "expected ci local merge to report no authorship, got: {output}"
    );
    assert!(
        repo.read_authorship_note(&merge_sha).is_none(),
        "expected no authorship note when source commits have no notes"
    );
}

#[test]
fn test_ci_rebase_merge_commit_order_pairing() {
    let repo = TestRepo::new();
    let base_sha = setup_main(&repo);

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut file_a = repo.filename("file_a.txt");
    file_a.set_contents(crate::lines!["ai content in file_a".ai()]);
    let feature_sha1 = repo.stage_all_and_commit("add file_a").unwrap().commit_sha;

    let mut file_b = repo.filename("file_b.txt");
    file_b.set_contents(crate::lines!["ai content in file_b".ai()]);
    let feature_sha2 = repo.stage_all_and_commit("add file_b").unwrap().commit_sha;

    repo.git_og(&["checkout", "main"]).unwrap();
    let mut main_only = repo.filename("main_only.txt");
    main_only.set_contents(crate::lines!["main-only content"]);
    repo.git_og(&["add", "main_only.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "advance main"]).unwrap();

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

    assert_ne!(new_sha1, feature_sha1);
    assert_ne!(new_sha2, feature_sha2);

    repo.git_og(&["checkout", "main"]).unwrap();
    repo.git_og(&["merge", "--ff-only", "feature"]).unwrap();

    let output = run_ci_local_merge(&repo, &new_sha2, &feature_sha2, &base_sha);
    assert_ci_rewrite_succeeded(&output);

    let files1 = authorship_files(&repo, &new_sha1);
    let files2 = authorship_files(&repo, &new_sha2);

    assert!(
        files1.iter().any(|file| file.contains("file_a")),
        "rebased commit 1 should reference file_a.txt, got: {files1:?}"
    );
    assert!(
        !files1.iter().any(|file| file.contains("file_b")),
        "rebased commit 1 should not reference file_b.txt, got: {files1:?}"
    );
    assert!(
        files2.iter().any(|file| file.contains("file_b")),
        "rebased commit 2 should reference file_b.txt, got: {files2:?}"
    );
    assert!(
        !files2.iter().any(|file| file.contains("file_a")),
        "rebased commit 2 should not reference file_a.txt, got: {files2:?}"
    );
}

crate::reuse_tests_in_worktree!(
    test_ci_squash_merge_basic,
    test_ci_squash_merge_multiple_files,
    test_ci_squash_merge_mixed_ai_and_human_content,
    test_ci_squash_merge_no_notes_no_authorship_created,
    test_ci_rebase_merge_commit_order_pairing,
);
