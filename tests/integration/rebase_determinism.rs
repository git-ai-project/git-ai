//! Rebase determinism and Git equivalence tests.
//!
//! These tests verify that git-ai's rebase hook is a transparent proxy that:
//! 1. Produces identical tree hashes to native git rebase (no file corruption)
//! 2. Generates deterministic commit SHAs with frozen environment variables
//! 3. Correctly maps line numbers through rebase operations
//! 4. Preserves AI attribution notes without affecting Git's core operations
//!
//! Unlike attribution-focused tests, these verify structural invariants and
//! prove git-ai doesn't interfere with Git's rebase mechanics.
//!
//! ## Cross-environment determinism
//!
//! To ensure identical tree/commit SHAs across CI runners and developer machines:
//! - Frozen environment variables control author/committer timestamps
//! - Explicit `core.autocrlf=false` prevents Windows CRLF conversions
//! - Explicit `core.filemode=false` ignores executable bit differences
//!
//! Without these, blob hashes differ between platforms → tree hash mismatch → SHA mismatch.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;

/// Frozen environment variables for deterministic Git commit SHAs.
/// With identical trees, parents, messages, and these frozen values,
/// Git will produce identical commit SHAs across test runs.
///
/// CRITICAL: GIT_COMMITTER_DATE must be passed to ALL git operations including
/// rebase. During rebase, Git preserves the original author date but updates
/// the committer date to "now" by default. Without frozen GIT_COMMITTER_DATE,
/// rebased commits would get different committer timestamps on each test run,
/// causing SHA mismatches even with identical content.
const FROZEN_ENV: &[(&str, &str)] = &[
    ("GIT_AUTHOR_DATE", "2024-01-01T00:00:00Z"),
    ("GIT_COMMITTER_DATE", "2024-01-01T00:00:00Z"),
    ("GIT_AUTHOR_NAME", "Test Author"),
    ("GIT_AUTHOR_EMAIL", "test@example.com"),
    ("GIT_COMMITTER_NAME", "Test Committer"),
    ("GIT_COMMITTER_EMAIL", "test@example.com"),
];

/// Set up a divergent fixture with a shared base commit, then separate
/// main and dev branches that modify the same file in non-conflicting ways.
///
/// Structure:
///   base: shared.rs with "fn base() {}"
///   main: adds "fn main_addition() {}" (prepended)
///   dev:  adds "fn dev_addition() {}" with AI attribution (appended)
///
/// After rebasing dev onto main, the result should have both additions.
fn setup_divergent_fixture(repo: &TestRepo) {
    // Ensure deterministic git config (prevent cross-environment blob/tree hash differences)
    repo.git(&["config", "core.autocrlf", "false"]).unwrap();
    repo.git(&["config", "core.filemode", "false"]).unwrap();

    // Shared base commit with two files
    let mut main_file = repo.filename("main_file.rs");
    let mut dev_file = repo.filename("dev_file.rs");
    main_file.set_contents(crate::lines!["fn base_main() {}"]);
    dev_file.set_contents(crate::lines!["fn base_dev() {}"]);
    repo.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo.git_with_env(&["commit", "-m", "Initial base commit"], FROZEN_ENV, None)
        .unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Create and advance main branch (modifies ONLY main_file.rs)
    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }
    main_file.set_contents(crate::lines!["fn base_main() {}", "fn main_addition() {}"]);
    repo.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo.git_with_env(&["commit", "-m", "Main diverges"], FROZEN_ENV, None)
        .unwrap();

    // Create dev branch from base (modifies ONLY dev_file.rs with AI)
    repo.git(&["checkout", "-b", "dev", &base_sha]).unwrap();
    dev_file.set_contents(crate::lines![
        "fn base_dev() {}",
        "fn dev_addition() {}".ai()
    ]);
    repo.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo.git_with_env(&["commit", "-m", "Dev adds AI feature"], FROZEN_ENV, None)
        .unwrap();
}

/// Set up a more complex multi-commit divergent fixture.
/// Tests that determinism and tree equivalence hold across multiple commits.
///
/// Structure:
///   base: lib.rs with module skeleton
///   main: 2 commits adding human functions
///   dev:  3 commits adding AI functions + new file
fn setup_multicommit_fixture(repo: &TestRepo) {
    // Ensure deterministic git config (prevent cross-environment blob/tree hash differences)
    repo.git(&["config", "core.autocrlf", "false"]).unwrap();
    repo.git(&["config", "core.filemode", "false"]).unwrap();

    // Base commit with separate files for main and dev
    let mut main_mod = repo.filename("main_mod.rs");
    let mut dev_mod = repo.filename("dev_mod.rs");
    main_mod.set_contents(crate::lines!["// Main module"]);
    dev_mod.set_contents(crate::lines!["// Dev module"]);
    repo.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo.git_with_env(&["commit", "-m", "Base library"], FROZEN_ENV, None)
        .unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Main branch: 2 commits (modifies ONLY main_mod.rs)
    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }
    main_mod.set_contents(crate::lines!["// Main module", "pub fn main_fn_1() {}"]);
    repo.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo.git_with_env(&["commit", "-m", "Main commit 1"], FROZEN_ENV, None)
        .unwrap();

    main_mod.set_contents(crate::lines![
        "// Main module",
        "pub fn main_fn_1() {}",
        "pub fn main_fn_2() {}"
    ]);
    repo.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo.git_with_env(&["commit", "-m", "Main commit 2"], FROZEN_ENV, None)
        .unwrap();

    // Dev branch: 3 commits with AI attribution (modifies ONLY dev_mod.rs + new file)
    repo.git(&["checkout", "-b", "dev", &base_sha]).unwrap();

    // Dev commit 1: AI adds function to dev_mod.rs
    dev_mod.set_contents(crate::lines![
        "// Dev module",
        "pub fn dev_ai_fn_1() {}".ai()
    ]);
    repo.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo.git_with_env(&["commit", "-m", "Dev commit 1: AI fn"], FROZEN_ENV, None)
        .unwrap();

    // Dev commit 2: AI adds another function
    dev_mod.set_contents(crate::lines![
        "// Dev module",
        "pub fn dev_ai_fn_1() {}".ai(),
        "pub fn dev_ai_fn_2() {}".ai()
    ]);
    repo.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo.git_with_env(&["commit", "-m", "Dev commit 2: AI fn 2"], FROZEN_ENV, None)
        .unwrap();

    // Dev commit 3: AI creates new file
    let mut helper = repo.filename("helper.rs");
    helper.set_contents(crate::lines!["pub fn helper() {}".ai()]);
    repo.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo.git_with_env(
        &["commit", "-m", "Dev commit 3: new file"],
        FROZEN_ENV,
        None,
    )
    .unwrap();
}

/// Get the N most-recent commit SHAs ordered oldest→newest.
fn get_commit_chain(repo: &TestRepo, n: usize) -> Vec<String> {
    (0..n)
        .rev()
        .map(|offset| {
            let rev = if offset == 0 {
                "HEAD".to_string()
            } else {
                format!("HEAD~{}", offset)
            };
            repo.git(&["rev-parse", &rev]).unwrap().trim().to_string()
        })
        .collect()
}

/// Verify that a git-ai rebase produces identical tree hashes and commit SHAs
/// to a native git rebase when using frozen environment variables.
#[test]
fn test_rebase_tree_equivalence_and_sha_determinism() {
    // === Run 1: git-ai rebase ===
    let repo1 = TestRepo::new_dedicated_daemon();
    setup_divergent_fixture(&repo1);

    let dev_tree_before_rebase = repo1
        .git(&["rev-parse", "dev^{tree}"])
        .unwrap()
        .trim()
        .to_string();

    repo1
        .git_with_env(&["checkout", "dev"], FROZEN_ENV, None)
        .unwrap();
    repo1
        .git_with_env(&["rebase", "main"], FROZEN_ENV, None)
        .unwrap();

    let ai_commits = get_commit_chain(&repo1, 1); // dev has 1 commit
    let ai_tree_after = repo1
        .git(&["rev-parse", "HEAD^{tree}"])
        .unwrap()
        .trim()
        .to_string();
    let ai_commit_sha = &ai_commits[0];

    // Verify AI note exists and contains expected attribution
    let note = repo1
        .read_authorship_note(ai_commit_sha)
        .expect("git-ai rebase should create authorship note");
    let log =
        AuthorshipLog::deserialize_from_string(&note).expect("note should parse as AuthorshipLog");
    assert!(
        !log.attestations.is_empty(),
        "rebase should preserve AI attestations"
    );

    // Verify line-level attribution correctness
    let mut main_file = repo1.filename("main_file.rs");
    main_file.assert_lines_and_blame(crate::lines!["fn base_main() {}", "fn main_addition() {}",]);

    let mut dev_file = repo1.filename("dev_file.rs");
    dev_file.assert_lines_and_blame(crate::lines![
        "fn base_dev() {}",
        "fn dev_addition() {}".ai(),
    ]);

    // === Run 2: native git rebase (bypassing git-ai hooks) ===
    let repo2 = TestRepo::new_dedicated_daemon();
    setup_divergent_fixture(&repo2);

    let native_tree_before = repo2
        .git_og(&["rev-parse", "dev^{tree}"])
        .unwrap()
        .trim()
        .to_string();

    repo2
        .git_og_with_env(&["checkout", "dev"], FROZEN_ENV)
        .unwrap();
    repo2
        .git_og_with_env(&["rebase", "main"], FROZEN_ENV)
        .unwrap();

    let native_commits = get_commit_chain(&repo2, 1);
    let native_tree_after = repo2
        .git_og(&["rev-parse", "HEAD^{tree}"])
        .unwrap()
        .trim()
        .to_string();
    let native_commit_sha = &native_commits[0];

    // === CRITICAL ASSERTIONS ===

    // 1. Tree hashes before rebase should be identical (same fixture)
    assert_eq!(
        dev_tree_before_rebase, native_tree_before,
        "fixture trees must match before rebase"
    );

    // 2. Tree hashes after rebase must match (git-ai didn't corrupt files)
    assert_eq!(
        ai_tree_after, native_tree_after,
        "TREE CORRUPTION: git-ai rebase produced different tree hash ({}) \
         than native git rebase ({}). This means git-ai modified file \
         contents beyond what Git naturally does during rebase.",
        ai_tree_after, native_tree_after
    );

    // 3. With frozen env, commit SHAs should be deterministic and identical
    assert_eq!(
        ai_commit_sha, native_commit_sha,
        "SHA DETERMINISM FAILURE: git-ai rebase produced commit SHA {} \
         but native rebase produced {}. With frozen environment variables, \
         identical trees and parents should produce identical SHAs.",
        ai_commit_sha, native_commit_sha
    );

    // 4. Run git-ai rebase again to verify determinism across multiple runs
    let repo3 = TestRepo::new();
    setup_divergent_fixture(&repo3);
    repo3
        .git_with_env(&["checkout", "dev"], FROZEN_ENV, None)
        .unwrap();
    repo3
        .git_with_env(&["rebase", "main"], FROZEN_ENV, None)
        .unwrap();
    let repeat_commits = get_commit_chain(&repo3, 1);

    assert_eq!(
        ai_commits, repeat_commits,
        "SHA INSTABILITY: git-ai rebase produced different commit SHA {} \
         on second run (first run: {}). Rebase should be deterministic with \
         frozen environment.",
        repeat_commits[0], ai_commits[0]
    );
}

/// Verify tree equivalence and SHA determinism with multiple commits on both branches.
/// This is a more rigorous test that ensures the invariants hold across complex histories.
#[test]
fn test_multicommit_rebase_tree_equivalence_and_sha_determinism() {
    // === Run 1: git-ai rebase (3 dev commits onto 2 main commits) ===
    let repo1 = TestRepo::new();
    setup_multicommit_fixture(&repo1);

    repo1
        .git_with_env(&["checkout", "dev"], FROZEN_ENV, None)
        .unwrap();
    repo1
        .git_with_env(&["rebase", "main"], FROZEN_ENV, None)
        .unwrap();

    let ai_commits = get_commit_chain(&repo1, 3); // 3 dev commits after rebase
    let ai_trees: Vec<String> = ai_commits
        .iter()
        .map(|sha| {
            repo1
                .git(&["rev-parse", &format!("{}^{{tree}}", sha)])
                .unwrap()
                .trim()
                .to_string()
        })
        .collect();

    // Verify all commits have authorship notes
    for sha in &ai_commits {
        let note = repo1
            .read_authorship_note(sha)
            .unwrap_or_else(|| panic!("commit {} should have authorship note after rebase", sha));
        assert!(
            !note.is_empty(),
            "note for commit {} should not be empty",
            sha
        );
    }

    // Verify final tree structure includes all changes
    let mut main_mod = repo1.filename("main_mod.rs");
    main_mod.assert_lines_and_blame(crate::lines![
        "// Main module",
        "pub fn main_fn_1() {}",
        "pub fn main_fn_2() {}",
    ]);

    let mut dev_mod = repo1.filename("dev_mod.rs");
    dev_mod.assert_lines_and_blame(crate::lines![
        "// Dev module",
        "pub fn dev_ai_fn_1() {}".ai(),
        "pub fn dev_ai_fn_2() {}".ai(),
    ]);

    let mut helper = repo1.filename("helper.rs");
    helper.assert_lines_and_blame(crate::lines!["pub fn helper() {}".ai()]);

    // === Run 2: native git rebase ===
    let repo2 = TestRepo::new();
    setup_multicommit_fixture(&repo2);

    repo2
        .git_og_with_env(&["checkout", "dev"], FROZEN_ENV)
        .unwrap();
    repo2
        .git_og_with_env(&["rebase", "main"], FROZEN_ENV)
        .unwrap();

    let native_commits = get_commit_chain(&repo2, 3);
    let native_trees: Vec<String> = native_commits
        .iter()
        .map(|sha| {
            repo2
                .git_og(&["rev-parse", &format!("{}^{{tree}}", sha)])
                .unwrap()
                .trim()
                .to_string()
        })
        .collect();

    // === ASSERTIONS ===

    // 1. Each rebased commit's tree must match native rebase
    for (i, (ai_tree, native_tree)) in ai_trees.iter().zip(&native_trees).enumerate() {
        assert_eq!(
            ai_tree,
            native_tree,
            "TREE MISMATCH at commit {}: git-ai produced {} but native git produced {}",
            i + 1,
            ai_tree,
            native_tree
        );
    }

    // 2. Each rebased commit's SHA must match (determinism)
    for (i, (ai_sha, native_sha)) in ai_commits.iter().zip(&native_commits).enumerate() {
        assert_eq!(
            ai_sha,
            native_sha,
            "SHA MISMATCH at commit {}: git-ai produced {} but native git produced {}. \
             With frozen environment, SHAs should be identical.",
            i + 1,
            ai_sha,
            native_sha
        );
    }

    // 3. Verify determinism by repeating git-ai rebase
    let repo3 = TestRepo::new();
    setup_multicommit_fixture(&repo3);
    repo3
        .git_with_env(&["checkout", "dev"], FROZEN_ENV, None)
        .unwrap();
    repo3
        .git_with_env(&["rebase", "main"], FROZEN_ENV, None)
        .unwrap();
    let repeat_commits = get_commit_chain(&repo3, 3);

    assert_eq!(
        ai_commits, repeat_commits,
        "INSTABILITY: git-ai rebase produced different SHAs on second run.\n\
         First run:  {:?}\n\
         Second run: {:?}",
        ai_commits, repeat_commits
    );
}

/// Verify that line number mapping through rebase is correct.
/// After rebasing, AI-attributed lines should still map to the correct content.
#[test]
fn test_line_number_mapping_through_rebase() {
    let repo = TestRepo::new_dedicated_daemon();

    // Ensure deterministic git config
    repo.git(&["config", "core.autocrlf", "false"]).unwrap();
    repo.git(&["config", "core.filemode", "false"]).unwrap();

    // Base: 10-line file
    let mut file = repo.filename("lines.txt");
    file.set_contents(crate::lines![
        "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8", "line 9",
        "line 10"
    ]);
    repo.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo.git_with_env(&["commit", "-m", "Base 10 lines"], FROZEN_ENV, None)
        .unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Main: prepend 3 lines (shifts everything down)
    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }
    file.set_contents(crate::lines![
        "main line 1",
        "main line 2",
        "main line 3",
        "line 1",
        "line 2",
        "line 3",
        "line 4",
        "line 5",
        "line 6",
        "line 7",
        "line 8",
        "line 9",
        "line 10"
    ]);
    repo.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo.git_with_env(&["commit", "-m", "Main prepends 3"], FROZEN_ENV, None)
        .unwrap();

    // Dev: append 2 AI lines at end
    repo.git(&["checkout", "-b", "dev", &base_sha]).unwrap();
    file.set_contents(crate::lines![
        "line 1",
        "line 2",
        "line 3",
        "line 4",
        "line 5",
        "line 6",
        "line 7",
        "line 8",
        "line 9",
        "line 10",
        "AI line 11".ai(),
        "AI line 12".ai()
    ]);
    repo.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo.git_with_env(&["commit", "-m", "Dev appends AI"], FROZEN_ENV, None)
        .unwrap();

    // Rebase dev onto main
    repo.git_with_env(&["rebase", "main"], FROZEN_ENV, None)
        .unwrap();

    // After rebase, AI lines should be at lines 14-15 (shifted by +3)
    file.assert_lines_and_blame(crate::lines![
        "main line 1",
        "main line 2",
        "main line 3",
        "line 1",
        "line 2",
        "line 3",
        "line 4",
        "line 5",
        "line 6",
        "line 7",
        "line 8",
        "line 9",
        "line 10",
        "AI line 11".ai(),
        "AI line 12".ai()
    ]);

    // Verify authorship note correctly records shifted line numbers
    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&rebased_sha)
        .expect("rebased commit should have note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("note should parse");

    // Find the attestation for lines.txt
    let attestation = log
        .attestations
        .iter()
        .find(|a| a.file_path == "lines.txt")
        .expect("note should contain lines.txt attestation");

    // Verify AI lines are attributed (entries with session hash)
    let ai_entries: Vec<_> = attestation
        .entries
        .iter()
        .filter(|e| e.hash.starts_with("s_"))
        .collect();
    assert!(
        !ai_entries.is_empty(),
        "should have at least one AI-attributed entry"
    );

    // Count total AI lines from line ranges
    let total_ai_lines: u32 = ai_entries
        .iter()
        .flat_map(|e| &e.line_ranges)
        .map(|r| match r {
            git_ai::authorship::authorship_log::LineRange::Single(_) => 1,
            git_ai::authorship::authorship_log::LineRange::Range(s, e) => e - s + 1,
        })
        .sum();

    assert_eq!(
        total_ai_lines, 2,
        "should have exactly 2 AI-attributed lines after rebase"
    );
}

/// Verify that rebase with file deletion + recreation produces correct trees.
/// Tests that git-ai doesn't interfere with Git's handling of file lifecycle.
#[test]
fn test_rebase_file_deletion_recreation_tree_equivalence() {
    // === Run 1: git-ai rebase ===
    let repo1 = TestRepo::new();

    // Ensure deterministic git config
    repo1.git(&["config", "core.autocrlf", "false"]).unwrap();
    repo1.git(&["config", "core.filemode", "false"]).unwrap();

    // Base: create temp.txt
    let mut temp = repo1.filename("temp.txt");
    temp.set_contents(crate::lines!["temporary content"]);
    repo1.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo1
        .git_with_env(&["commit", "-m", "Add temp.txt"], FROZEN_ENV, None)
        .unwrap();

    let base_sha = repo1
        .git(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Main: delete temp.txt
    let default_branch = repo1.current_branch();
    if default_branch != "main" {
        repo1.git(&["branch", "-M", "main"]).unwrap();
    }
    repo1
        .git_with_env(&["rm", "temp.txt"], FROZEN_ENV, None)
        .unwrap();
    repo1
        .git_with_env(&["commit", "-m", "Delete temp.txt"], FROZEN_ENV, None)
        .unwrap();

    // Dev: recreate temp.txt with different AI content
    repo1.git(&["checkout", "-b", "dev", &base_sha]).unwrap();
    repo1
        .git_with_env(&["rm", "temp.txt"], FROZEN_ENV, None)
        .unwrap();
    repo1
        .git_with_env(&["commit", "-m", "Dev deletes temp.txt"], FROZEN_ENV, None)
        .unwrap();

    temp.set_contents(crate::lines!["new AI content".ai()]);
    repo1.git_with_env(&["add", "."], FROZEN_ENV, None).unwrap();
    repo1
        .git_with_env(&["commit", "-m", "Dev recreates with AI"], FROZEN_ENV, None)
        .unwrap();

    // Rebase dev onto main
    repo1
        .git_with_env(&["checkout", "dev"], FROZEN_ENV, None)
        .unwrap();
    repo1
        .git_with_env(&["rebase", "main"], FROZEN_ENV, None)
        .unwrap();

    let ai_commits = get_commit_chain(&repo1, 2);
    let ai_final_tree = repo1
        .git(&["rev-parse", "HEAD^{tree}"])
        .unwrap()
        .trim()
        .to_string();

    // === Run 2: native git rebase ===
    let repo2 = TestRepo::new();

    // Ensure deterministic git config (same as repo1)
    repo2.git_og(&["config", "core.autocrlf", "false"]).unwrap();
    repo2.git_og(&["config", "core.filemode", "false"]).unwrap();

    let mut temp2 = repo2.filename("temp.txt");
    temp2.set_contents(crate::lines!["temporary content"]);
    repo2.git_og_with_env(&["add", "."], FROZEN_ENV).unwrap();
    repo2
        .git_og_with_env(&["commit", "-m", "Add temp.txt"], FROZEN_ENV)
        .unwrap();

    let base_sha2 = repo2
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let default_branch2 = repo2.current_branch();
    if default_branch2 != "main" {
        repo2.git_og(&["branch", "-M", "main"]).unwrap();
    }
    repo2
        .git_og_with_env(&["rm", "temp.txt"], FROZEN_ENV)
        .unwrap();
    repo2
        .git_og_with_env(&["commit", "-m", "Delete temp.txt"], FROZEN_ENV)
        .unwrap();

    repo2
        .git_og(&["checkout", "-b", "dev", &base_sha2])
        .unwrap();
    repo2
        .git_og_with_env(&["rm", "temp.txt"], FROZEN_ENV)
        .unwrap();
    repo2
        .git_og_with_env(&["commit", "-m", "Dev deletes temp.txt"], FROZEN_ENV)
        .unwrap();

    temp2.set_contents(crate::lines!["new AI content"]);
    repo2.git_og_with_env(&["add", "."], FROZEN_ENV).unwrap();
    repo2
        .git_og_with_env(&["commit", "-m", "Dev recreates with AI"], FROZEN_ENV)
        .unwrap();

    repo2
        .git_og_with_env(&["checkout", "dev"], FROZEN_ENV)
        .unwrap();
    repo2
        .git_og_with_env(&["rebase", "main"], FROZEN_ENV)
        .unwrap();

    let native_commits = get_commit_chain(&repo2, 2);
    let native_final_tree = repo2
        .git_og(&["rev-parse", "HEAD^{tree}"])
        .unwrap()
        .trim()
        .to_string();

    // === ASSERTIONS ===
    assert_eq!(
        ai_final_tree, native_final_tree,
        "TREE MISMATCH after file deletion+recreation: git-ai produced {} but native produced {}",
        ai_final_tree, native_final_tree
    );

    for (i, (ai_sha, native_sha)) in ai_commits.iter().zip(&native_commits).enumerate() {
        assert_eq!(
            ai_sha,
            native_sha,
            "SHA MISMATCH at commit {} after file deletion+recreation",
            i + 1
        );
    }

    // Verify final state
    temp.assert_lines_and_blame(crate::lines!["new AI content".ai()]);
}

crate::reuse_tests_in_worktree!(
    test_rebase_tree_equivalence_and_sha_determinism,
    test_multicommit_rebase_tree_equivalence_and_sha_determinism,
    test_line_number_mapping_through_rebase,
    test_rebase_file_deletion_recreation_tree_equivalence,
);
