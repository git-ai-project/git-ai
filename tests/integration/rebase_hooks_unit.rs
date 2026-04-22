use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::git::cli_parser::summarize_rebase_args;

// ─── Helper ────────────────────────────────────────────────────────────────
/// Build a `command_args` slice as `summarize_rebase_args` expects (args after
/// the "rebase" command word).
fn args(raw: &[&str]) -> Vec<String> {
    raw.iter().map(|s| s.to_string()).collect()
}

// ─── Pure arg-parsing tests ────────────────────────────────────────────────
// These exercise `summarize_rebase_args` directly — no git repo required.

#[test]
fn test_summarize_rebase_args_continue_is_control_mode() {
    let summary = summarize_rebase_args(&args(&["--continue"]));
    assert!(summary.is_control_mode);
}

#[test]
fn test_summarize_rebase_args_abort_is_control_mode() {
    let summary = summarize_rebase_args(&args(&["--abort"]));
    assert!(summary.is_control_mode);
}

#[test]
fn test_summarize_rebase_args_skip_is_control_mode() {
    let summary = summarize_rebase_args(&args(&["--skip"]));
    assert!(summary.is_control_mode);
}

#[test]
fn test_summarize_rebase_args_upstream_only() {
    let summary = summarize_rebase_args(&args(&["origin/main"]));
    assert!(!summary.is_control_mode);
    assert_eq!(summary.positionals, vec!["origin/main".to_string()]);
}

#[test]
fn test_summarize_rebase_args_upstream_and_branch() {
    let summary = summarize_rebase_args(&args(&["origin/main", "feature"]));
    assert!(!summary.is_control_mode);
    assert_eq!(
        summary.positionals,
        vec!["origin/main".to_string(), "feature".to_string()]
    );
}

#[test]
fn test_summarize_rebase_args_onto_flag() {
    let summary = summarize_rebase_args(&args(&["--onto", "abc123", "origin/main"]));
    assert!(!summary.is_control_mode);
    assert_eq!(summary.onto_spec, Some("abc123".to_string()));
    assert_eq!(summary.positionals, vec!["origin/main".to_string()]);
}

#[test]
fn test_summarize_rebase_args_onto_equals_flag() {
    let summary = summarize_rebase_args(&args(&["--onto=abc123", "origin/main"]));
    assert!(!summary.is_control_mode);
    assert_eq!(summary.onto_spec, Some("abc123".to_string()));
}

#[test]
fn test_summarize_rebase_args_root_flag() {
    let summary = summarize_rebase_args(&args(&["--root"]));
    assert!(!summary.is_control_mode);
    assert!(summary.has_root);
}

#[test]
fn test_summarize_rebase_args_interactive_with_upstream() {
    let summary = summarize_rebase_args(&args(&["-i", "origin/main"]));
    assert!(!summary.is_control_mode);
    assert_eq!(summary.positionals, vec!["origin/main".to_string()]);
}

#[test]
fn test_summarize_rebase_args_strategy_consumes_value() {
    let summary = summarize_rebase_args(&args(&["-s", "ours", "origin/main"]));
    assert!(!summary.is_control_mode);
    assert_eq!(summary.positionals, vec!["origin/main".to_string()]);
}

// ─── build_rebase_commit_mappings tests ────────────────────────────────────
// These replicate the TmpRepo-based unit tests using the TestRepo harness.
// Each sets up the same branch topology (base + side merge on default branch,
// feature branch from base), rebases through the wrapper, then calls
// build_rebase_commit_mappings to verify commit mapping correctness.

/// Helper: set up a repo with a merge commit on the default branch.
///
/// Topology after setup (on the default branch):
///   base ── main_commit ── Merge(side) = default HEAD
///             └── side_commit ──┘
///
/// Returns `(default_branch_name, base_sha, merge_sha)`.
fn setup_merge_on_default(repo: &TestRepo) -> (String, String, String) {
    let mut base = repo.filename("base.txt");
    base.set_contents(vec!["base".human()]);
    repo.stage_all_and_commit("base commit")
        .expect("base commit");
    let base_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("base sha")
        .trim()
        .to_string();
    let default_branch = repo.current_branch();

    // Side branch with a commit
    repo.git(&["checkout", "-b", "side"]).expect("create side");
    let mut side = repo.filename("side.txt");
    side.set_contents(vec!["side".human()]);
    repo.stage_all_and_commit("side commit")
        .expect("side commit");

    // Back to default, add a commit, merge --no-ff
    repo.git(&["checkout", &default_branch])
        .expect("switch to default");
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(vec!["main".human()]);
    repo.stage_all_and_commit("main commit")
        .expect("main commit");

    repo.git(&["merge", "--no-ff", "side", "-m", "Merge side"])
        .expect("merge");
    let merge_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("merge sha")
        .trim()
        .to_string();

    (default_branch, base_sha, merge_sha)
}

/// Migrated from `test_build_rebase_commit_mappings_excludes_merge_commits_from_new_commits`.
///
/// Creates a merge commit on the default branch, then rebases a feature branch
/// (with AI content) onto it.  Calls `build_rebase_commit_mappings` with
/// `onto_head = None` (the daemon fallback path) and verifies the merge commit
/// is NOT included in new_commits, and there is exactly 1 original / 1 new.
#[test]
fn test_build_rebase_commit_mappings_excludes_merge_commits_from_new_commits() {
    let repo = TestRepo::new();
    let (default_branch, base_sha, merge_sha) = setup_merge_on_default(&repo);

    // Feature branch from base with AI content
    repo.git(&["checkout", "-b", "feature", &base_sha])
        .expect("create feature");
    let mut ai_file = repo.filename("feat.txt");
    ai_file.set_contents(vec!["AI feature line".ai()]);
    repo.stage_all_and_commit("feature commit")
        .expect("feature commit");
    let original_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("original head")
        .trim()
        .to_string();

    // Rebase through the wrapper
    repo.git(&["rebase", &default_branch]).expect("rebase");
    let new_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("new head")
        .trim()
        .to_string();

    // Call build_rebase_commit_mappings with onto_head = None (daemon fallback)
    let path_str = repo.path().to_str().expect("valid path");
    let gitai_repo = git_ai::git::repository::find_repository_in_path(path_str).expect("open repo");
    let (original_commits, new_commits) =
        git_ai::commands::hooks::rebase_hooks::build_rebase_commit_mappings(
            &gitai_repo,
            &original_head,
            &new_head,
            None,
        )
        .expect("build mappings");

    assert!(
        !new_commits.contains(&merge_sha),
        "new_commits should not contain the merge commit {}, but got: {:?}",
        merge_sha,
        new_commits
    );
    assert_eq!(
        original_commits.len(),
        1,
        "Should have exactly 1 original commit, got: {:?}",
        original_commits
    );
    assert_eq!(
        new_commits.len(),
        1,
        "Should have exactly 1 new commit, got: {:?}",
        new_commits
    );

    // Verify AI authorship survived the rebase
    ai_file.assert_lines_and_blame(vec!["AI feature line".ai()]);
}

/// Migrated from `test_build_rebase_commit_mappings_excludes_merge_commits_when_onto_equals_merge_base`.
///
/// Same topology, but passes `onto_head = Some(merge_base)` to simulate the
/// daemon fallback where onto_head happens to equal the merge base.
#[test]
fn test_build_rebase_commit_mappings_excludes_merge_commits_when_onto_equals_merge_base() {
    let repo = TestRepo::new();
    let (default_branch, base_sha, merge_sha) = setup_merge_on_default(&repo);

    repo.git(&["checkout", "-b", "feature", &base_sha])
        .expect("create feature");
    let mut ai_file = repo.filename("feat.txt");
    ai_file.set_contents(vec!["AI feature line".ai()]);
    repo.stage_all_and_commit("feature commit")
        .expect("feature commit");
    let original_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("original head")
        .trim()
        .to_string();

    repo.git(&["rebase", &default_branch]).expect("rebase");
    let new_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("new head")
        .trim()
        .to_string();

    let merge_base_sha = repo
        .git(&["merge-base", &original_head, &new_head])
        .expect("merge-base")
        .trim()
        .to_string();

    let path_str = repo.path().to_str().expect("valid path");
    let gitai_repo = git_ai::git::repository::find_repository_in_path(path_str).expect("open repo");
    let (original_commits, new_commits) =
        git_ai::commands::hooks::rebase_hooks::build_rebase_commit_mappings(
            &gitai_repo,
            &original_head,
            &new_head,
            Some(&merge_base_sha),
        )
        .expect("build mappings");

    assert!(
        !new_commits.contains(&merge_sha),
        "new_commits should not contain merge commit {} when onto_head == merge_base, got: {:?}",
        merge_sha,
        new_commits
    );
    assert_eq!(original_commits.len(), 1);
    assert_eq!(new_commits.len(), 1);

    ai_file.assert_lines_and_blame(vec!["AI feature line".ai()]);
}

/// Migrated from `test_build_rebase_commit_mappings_multi_commit_with_onto_equals_merge_base`.
///
/// Same topology but with 2 feature commits.  Verifies 2 original and 2 new
/// commits in the mapping.
#[test]
fn test_build_rebase_commit_mappings_multi_commit_with_onto_equals_merge_base() {
    let repo = TestRepo::new();
    let (default_branch, base_sha, _merge_sha) = setup_merge_on_default(&repo);

    repo.git(&["checkout", "-b", "feature", &base_sha])
        .expect("create feature");

    let mut ai_file1 = repo.filename("feat1.txt");
    ai_file1.set_contents(vec!["AI feat1 line".ai()]);
    repo.stage_all_and_commit("feature commit 1")
        .expect("feature commit 1");

    let mut ai_file2 = repo.filename("feat2.txt");
    ai_file2.set_contents(vec!["AI feat2 line".ai()]);
    repo.stage_all_and_commit("feature commit 2")
        .expect("feature commit 2");

    let original_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("original head")
        .trim()
        .to_string();

    repo.git(&["rebase", &default_branch]).expect("rebase");
    let new_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("new head")
        .trim()
        .to_string();

    let merge_base_sha = repo
        .git(&["merge-base", &original_head, &new_head])
        .expect("merge-base")
        .trim()
        .to_string();

    let path_str = repo.path().to_str().expect("valid path");
    let gitai_repo = git_ai::git::repository::find_repository_in_path(path_str).expect("open repo");
    let (original_commits, new_commits) =
        git_ai::commands::hooks::rebase_hooks::build_rebase_commit_mappings(
            &gitai_repo,
            &original_head,
            &new_head,
            Some(&merge_base_sha),
        )
        .expect("build mappings");

    assert_eq!(
        original_commits.len(),
        2,
        "Should have 2 original commits, got: {:?}",
        original_commits
    );
    assert_eq!(
        new_commits.len(),
        2,
        "Should have 2 new commits, got: {:?}",
        new_commits
    );

    // Verify AI authorship survived
    ai_file1.assert_lines_and_blame(vec!["AI feat1 line".ai()]);
    ai_file2.assert_lines_and_blame(vec!["AI feat2 line".ai()]);
}

// Only the tests that use TestRepo need worktree variants.
// The pure arg-parsing tests have no repo interaction.
crate::reuse_tests_in_worktree!(
    test_build_rebase_commit_mappings_excludes_merge_commits_from_new_commits,
    test_build_rebase_commit_mappings_excludes_merge_commits_when_onto_equals_merge_base,
    test_build_rebase_commit_mappings_multi_commit_with_onto_equals_merge_base,
);
