use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::git::refs::notes_add;
use git_ai::git::repository as GitAiRepository;

fn direct_test_repo() -> TestRepo {
    TestRepo::new()
}

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

#[test]
fn test_ci_local_sync_skips_when_current_rebased_commit_already_has_note() {
    let repo = direct_test_repo();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    repo.git_og(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["ai content".ai()]);
    let previous_head_sha = repo.stage_all_and_commit("Add feature").unwrap().commit_sha;

    repo.git_og(&["checkout", "main"]).unwrap();
    let mut main_file = repo.filename("main_only.txt");
    main_file.set_contents(crate::lines!["main-only content"]);
    repo.git_og(&["add", "main_only.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "Advance main"]).unwrap();
    let base_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    repo.git_og(&["checkout", "feature"]).unwrap();
    repo.git_og(&["rebase", "main"]).unwrap();
    let current_head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo =
        GitAiRepository::find_repository_in_path(repo.path().to_str().expect("repo path"))
            .expect("git-ai repo");
    let existing_note = "client-side-note-that-ci-must-not-overwrite";
    notes_add(&gitai_repo, &current_head_sha, existing_note).expect("add existing current note");

    let output = repo
        .git_ai(&[
            "ci",
            "local",
            "sync",
            "--previous-head-sha",
            previous_head_sha.as_str(),
            "--base-ref",
            "main",
            "--base-sha",
            base_sha.as_str(),
            "--head-sha",
            current_head_sha.as_str(),
            "--skip-fetch-notes",
            "--skip-push",
        ])
        .expect("ci local sync should succeed");

    assert!(
        output.contains("Local CI (sync): skipped PR sync with existing authorship"),
        "Expected existing-note skip, got: {}",
        output
    );
    let current_note = repo
        .read_authorship_note(&current_head_sha)
        .map(|note| note.trim().to_string());
    assert_eq!(
        current_note.as_deref(),
        Some(existing_note),
        "CI sync must not overwrite a current commit note that already exists"
    );
}

#[test]
fn test_ci_local_sync_skips_non_rebase_force_push() {
    let repo = direct_test_repo();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    repo.git_og(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["old ai content".ai()]);
    let previous_head_sha = repo
        .stage_all_and_commit("Add old AI content")
        .unwrap()
        .commit_sha;
    assert!(
        repo.read_authorship_note(&previous_head_sha).is_some(),
        "old PR head should have an authorship note"
    );

    repo.git_og(&["reset", "--hard", "main"]).unwrap();
    feature_file.set_contents(crate::lines!["different force-pushed content"]);
    repo.git_og(&["add", "feature.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "Force-pushed replacement"])
        .unwrap();
    let current_head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let output = repo
        .git_ai(&[
            "ci",
            "local",
            "sync",
            "--previous-head-sha",
            previous_head_sha.as_str(),
            "--base-ref",
            "main",
            "--head-sha",
            current_head_sha.as_str(),
            "--skip-fetch-notes",
            "--skip-fetch-sync-refs",
            "--skip-push",
        ])
        .expect("ci local sync should succeed for non-rebase force push");

    assert!(
        output.contains("Local CI (sync): skipped non-rebase PR sync"),
        "Expected non-rebase sync skip, got: {}",
        output
    );
    assert!(
        repo.read_authorship_note(&current_head_sha).is_none(),
        "non-rebase sync must not transfer old authorship to unrelated replacement commit"
    );
}

#[test]
fn test_ci_local_open_pr_rebase_single_commit() {
    use git_ai::authorship::authorship_log_serialization::AuthorshipLog;

    let repo = direct_test_repo();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    repo.git_og(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["ai content".ai()]);
    let previous_head_sha = repo.stage_all_and_commit("Add feature").unwrap().commit_sha;

    repo.git_og(&["checkout", "main"]).unwrap();
    let mut main_file = repo.filename("main_only.txt");
    main_file.set_contents(crate::lines!["main-only content"]);
    repo.git_og(&["add", "main_only.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "Advance main"]).unwrap();
    let base_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    repo.git_og(&["checkout", "feature"]).unwrap();
    repo.git_og(&["rebase", "main"]).unwrap();
    let current_head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    assert_ne!(current_head_sha, previous_head_sha);
    assert!(
        repo.read_authorship_note(&current_head_sha).is_none(),
        "bypassed rebase should not pre-create note for the rebased commit"
    );

    let output = repo
        .git_ai(&[
            "ci",
            "local",
            "sync",
            "--previous-head-sha",
            previous_head_sha.as_str(),
            "--base-ref",
            "main",
            "--base-sha",
            base_sha.as_str(),
            "--head-sha",
            current_head_sha.as_str(),
            "--skip-fetch-notes",
            "--skip-push",
        ])
        .expect("ci local sync should succeed");

    assert!(
        output.contains("Local CI (sync): authorship rewritten successfully"),
        "Expected authorship rewritten, got: {}",
        output
    );

    let note = repo
        .read_authorship_note(&current_head_sha)
        .expect("rebased single PR commit should have an authorship note");
    let files: Vec<String> = AuthorshipLog::deserialize_from_string(&note)
        .unwrap()
        .attestations
        .iter()
        .map(|a| a.file_path.clone())
        .collect();
    assert!(
        files.iter().any(|f| f.contains("feature.txt")),
        "rebased single PR commit should reference feature.txt, got: {:?}",
        files
    );
}

#[test]
fn test_ci_local_open_pr_rebase_two_commits() {
    use git_ai::authorship::authorship_log_serialization::AuthorshipLog;

    let repo = direct_test_repo();

    // --- Initial commit on main ---
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();

    // --- Feature branch: two AI commits touching distinct files ---
    repo.git_og(&["checkout", "-b", "feature"]).unwrap();

    let mut file_a = repo.filename("file_a.txt");
    file_a.set_contents(crate::lines!["ai content in file_a".ai()]);
    let feature_sha1 = repo.stage_all_and_commit("Add file_a").unwrap().commit_sha;

    let mut file_b = repo.filename("file_b.txt");
    file_b.set_contents(crate::lines!["ai content in file_b".ai()]);
    let feature_sha2 = repo.stage_all_and_commit("Add file_b").unwrap().commit_sha;

    let previous_head_sha = feature_sha2.clone();

    // --- Advance main so the open-PR rebase produces new SHAs ---
    repo.git_og(&["checkout", "main"]).unwrap();
    let mut main_file = repo.filename("main_only.txt");
    main_file.set_contents(crate::lines!["main-only content"]);
    repo.git_og(&["add", "main_only.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "Advance main"]).unwrap();
    let base_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // --- Rebase the open feature branch onto main, bypassing local hooks ---
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

    assert_ne!(
        new_sha1, feature_sha1,
        "open-PR rebase must produce a new SHA for commit 1"
    );
    assert_ne!(
        new_sha2, feature_sha2,
        "open-PR rebase must produce a new SHA for commit 2"
    );
    assert!(
        repo.read_authorship_note(&new_sha1).is_none(),
        "bypassed rebase should not pre-create note for commit 1"
    );
    assert!(
        repo.read_authorship_note(&new_sha2).is_none(),
        "bypassed rebase should not pre-create note for commit 2"
    );

    // --- Run the new open-PR sync command ---
    let output = repo
        .git_ai(&[
            "ci",
            "local",
            "sync",
            "--previous-head-sha",
            previous_head_sha.as_str(),
            "--base-ref",
            "main",
            "--base-sha",
            base_sha.as_str(),
            "--head-sha",
            new_sha2.as_str(),
            "--skip-fetch-notes",
            "--skip-push",
        ])
        .expect("ci local sync should succeed");

    assert!(
        output.contains("Local CI (sync): authorship rewritten successfully"),
        "Expected authorship rewritten, got: {}",
        output
    );

    // --- Verify each rebased open-PR commit carries notes for its own file ---
    let note1 = repo
        .read_authorship_note(&new_sha1)
        .expect("rebased PR commit 1 should have an authorship note");
    let note2 = repo
        .read_authorship_note(&new_sha2)
        .expect("rebased PR commit 2 should have an authorship note");

    let files1: Vec<String> = AuthorshipLog::deserialize_from_string(&note1)
        .unwrap()
        .attestations
        .iter()
        .map(|a| a.file_path.clone())
        .collect();
    let files2: Vec<String> = AuthorshipLog::deserialize_from_string(&note2)
        .unwrap()
        .attestations
        .iter()
        .map(|a| a.file_path.clone())
        .collect();

    assert!(
        files1.iter().any(|f| f.contains("file_a")),
        "rebased PR commit 1 should reference file_a.txt, got: {:?}",
        files1
    );
    assert!(
        !files1.iter().any(|f| f.contains("file_b")),
        "rebased PR commit 1 should not reference file_b.txt, got: {:?}",
        files1
    );
    assert!(
        files2.iter().any(|f| f.contains("file_b")),
        "rebased PR commit 2 should reference file_b.txt, got: {:?}",
        files2
    );
    assert!(
        !files2.iter().any(|f| f.contains("file_a")),
        "rebased PR commit 2 should not reference file_a.txt, got: {:?}",
        files2
    );
}

#[test]
fn test_ci_local_merge_squash_on_linear_main_does_not_note_base_commits() {
    let repo = direct_test_repo();
    repo.git_og(&["config", "user.name", "Test User"]).unwrap();
    repo.git_og(&["config", "user.email", "test@example.com"])
        .unwrap();

    // B0: initial commit on main (raw git -> no authorship note)
    std::fs::write(repo.path().join("base.txt"), "base content\n").unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "B0 initial"]).unwrap();
    repo.git_og(&["branch", "-M", "main"]).unwrap();
    let b0_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // B1, B2, B3: teammate commits on main, NOT using the wrapper (no notes)
    for i in 1..=3 {
        std::fs::write(
            repo.path().join(format!("teammate{i}.txt")),
            format!("teammate change {i}\n"),
        )
        .unwrap();
        repo.git_og(&["add", "-A"]).unwrap();
        repo.git_og(&["commit", "-m", &format!("B{i} teammate change")])
            .unwrap();
    }
    let b2_sha = repo
        .git_og(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    let b3_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // feature branch off B0 with 3 AI commits (each gets a note via the wrapper)
    repo.git_og(&["checkout", "-b", "feature", &b0_sha])
        .unwrap();
    let mut feat = repo.filename("feature.txt");
    feat.set_contents(crate::lines!["// P1 ai line".ai()]);
    repo.stage_all_and_commit("P1").unwrap();
    feat.insert_at(1, crate::lines!["// P2 ai line".ai()]);
    repo.stage_all_and_commit("P2").unwrap();
    feat.insert_at(2, crate::lines!["// P3 ai line".ai()]);
    let head_sha = repo.stage_all_and_commit("P3").unwrap().commit_sha;

    // Squash merge: GitHub creates one new commit S on top of B3 (raw git)
    repo.git_og(&["checkout", "main"]).unwrap();
    std::fs::write(
        repo.path().join("feature.txt"),
        "// P1 ai line\n// P2 ai line\n// P3 ai line\n",
    )
    .unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "Squash merge feature (#PR)"])
        .unwrap();
    let squash_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Bare origin so `ci local merge` can push authorship
    let origin_dir = tempfile::tempdir().unwrap();
    let origin_path = origin_dir.path().join("origin.git");
    repo.git_og(&[
        "clone",
        "--bare",
        repo.path().to_str().unwrap(),
        origin_path.to_str().unwrap(),
    ])
    .unwrap();
    repo.git_og(&["remote", "add", "origin", origin_path.to_str().unwrap()])
        .unwrap();

    // Run the real CLI exactly as CI would after a squash merge
    let output = repo
        .git_ai(&[
            "ci",
            "local",
            "merge",
            "--merge-commit-sha",
            squash_sha.as_str(),
            "--head-ref",
            "feature",
            "--head-sha",
            head_sha.as_str(),
            "--base-ref",
            "main",
            "--base-sha",
            b3_sha.as_str(),
            "--skip-fetch-notes",
            "--skip-fetch-base",
        ])
        .expect("ci local merge should succeed");

    assert!(
        output.contains("authorship rewritten successfully"),
        "expected authorship rewritten, got: {output}"
    );

    // Only the squash commit S carries a note; the base commits are untouched.
    assert!(
        repo.read_authorship_note(&squash_sha).is_some(),
        "squash commit S ({squash_sha}) should receive the rewritten authorship note"
    );
    assert!(
        repo.read_authorship_note(&b2_sha).is_none(),
        "#1473 regression: unrelated base commit B2 ({b2_sha}) must not receive a note"
    );
    assert!(
        repo.read_authorship_note(&b3_sha).is_none(),
        "#1473 regression: unrelated base commit B3 ({b3_sha}) must not receive a note"
    );
}

#[test]
fn test_ci_local_rebase_merge_with_abbreviated_merge_sha() {
    let repo = direct_test_repo();

    // --- Initial commit on main ---
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    repo.git(&["branch", "-M", "main"]).unwrap();
    let base_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // --- Feature branch: two commits touching different files ---
    repo.git_og(&["checkout", "-b", "feature"]).unwrap();
    let mut file_a = repo.filename("file_a.txt");
    file_a.set_contents(crate::lines!["ai content in file_a".ai()]);
    let _feature_sha1 = repo.stage_all_and_commit("Add file_a").unwrap().commit_sha;
    let mut file_b = repo.filename("file_b.txt");
    file_b.set_contents(crate::lines!["ai content in file_b".ai()]);
    let feature_sha2 = repo.stage_all_and_commit("Add file_b").unwrap().commit_sha;

    // --- Advance main so the rebase produces new commit SHAs ---
    repo.git_og(&["checkout", "main"]).unwrap();
    let mut main_file = repo.filename("main_only.txt");
    main_file.set_contents(crate::lines!["main-only content"]);
    repo.git_og(&["add", "main_only.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "Advance main"]).unwrap();

    // --- Rebase feature onto main (bypassing the local hook), then ff main ---
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
    repo.git_og(&["checkout", "main"]).unwrap();
    repo.git_og(&["merge", "--ff-only", "feature"]).unwrap();

    // --- Bare origin so push_authorship inside CiContext can succeed ---
    let origin_dir = tempfile::tempdir().unwrap();
    let origin_path = origin_dir.path().join("origin.git");
    repo.git_og(&[
        "clone",
        "--bare",
        repo.path().to_str().unwrap(),
        origin_path.to_str().unwrap(),
    ])
    .unwrap();
    repo.git_og(&["remote", "add", "origin", origin_path.to_str().unwrap()])
        .unwrap();

    // --- Run `ci local merge` with an ABBREVIATED merge-commit-sha ---
    let abbreviated_merge_sha = &new_sha2[..12];
    let output = repo
        .git_ai(&[
            "ci",
            "local",
            "merge",
            "--merge-commit-sha",
            abbreviated_merge_sha,
            "--head-ref",
            "feature",
            "--head-sha",
            feature_sha2.as_str(),
            "--base-ref",
            "main",
            "--base-sha",
            base_sha.as_str(),
            "--skip-fetch-notes",
            "--skip-fetch-base",
        ])
        .expect("ci local merge should succeed");

    assert!(
        output.contains("authorship rewritten successfully"),
        "expected authorship rewritten, got: {output}"
    );

    // --- Each rebased commit must still carry its own note (rebase path kept) ---
    let note1 = repo
        .read_authorship_note(&new_sha1)
        .expect("rebased commit 1 should have a note (rebase must not be misclassified as squash)");
    let note2 = repo
        .read_authorship_note(&new_sha2)
        .expect("rebased commit 2 should have a note");

    let files = |note: &str| -> Vec<String> {
        AuthorshipLog::deserialize_from_string(note)
            .unwrap()
            .attestations
            .iter()
            .map(|a| a.file_path.clone())
            .collect()
    };
    let files1 = files(&note1);
    let files2 = files(&note2);

    assert!(
        files1.iter().any(|f| f.contains("file_a")) && !files1.iter().any(|f| f.contains("file_b")),
        "rebased commit 1 should reference only file_a.txt, got: {files1:?}"
    );
    assert!(
        files2.iter().any(|f| f.contains("file_b")) && !files2.iter().any(|f| f.contains("file_a")),
        "rebased commit 2 should reference only file_b.txt, got: {files2:?}"
    );
}

crate::reuse_tests_in_worktree!(
    test_ci_squash_merge_basic,
    test_ci_squash_merge_multiple_files,
    test_ci_squash_merge_mixed_ai_and_human_content,
    test_ci_squash_merge_no_notes_no_authorship_created,
    test_ci_rebase_merge_commit_order_pairing,
    test_ci_local_sync_skips_when_current_rebased_commit_already_has_note,
    test_ci_local_sync_skips_non_rebase_force_push,
    test_ci_local_open_pr_rebase_single_commit,
    test_ci_local_open_pr_rebase_two_commits,
    test_ci_local_merge_squash_on_linear_main_does_not_note_base_commits,
    test_ci_local_rebase_merge_with_abbreviated_merge_sha,
);
