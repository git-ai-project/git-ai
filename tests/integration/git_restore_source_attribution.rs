//! Attribution preservation across `git restore --source <commit>`.
//!
//! Reproduces a real-world stacked-PR workflow where all AI editing happens on a
//! single "squash" branch (with proper checkpoints), and the work is then split
//! into a stack of focused commits on fresh branches by pulling file content out
//! of the squash commit via `git restore --source <squash> -- <files>` followed
//! by a plain `git commit` -- with NO checkpoint between the restore and the
//! commit.
//!
//! Because `git restore` writes the worktree/index without firing a checkpoint,
//! the restored content arrives "invisibly" and the resulting commits are
//! attributed as 100% untracked even though the byte-identical content was AI
//! authored on the squash commit (which carries a correct AI authorship note).
//!
//! All data here is synthetic.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// AI-authored content carried into a new commit via `git restore --source`
/// should remain AI-attributed, not collapse to untracked.
#[test]
fn test_restore_source_preserves_ai_attribution() {
    let repo = TestRepo::new();

    // Base commit on the default branch -- this stands in for `origin/master`,
    // the base the stacked PR branches are created from.
    let base_path = repo.path().join("base.txt");
    fs::write(&base_path, "base line\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let main_branch = repo.current_branch();

    // --- Phase A: all AI work happens on a single "squash" branch ---
    repo.git(&["switch", "-c", "squash", &main_branch]).unwrap();
    let feature_path = repo.path().join("feature.ts");
    fs::write(
        &feature_path,
        "export const feature = () => \"written by AI\";\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "feature.ts"])
        .unwrap();
    repo.stage_all_and_commit("Migrate feature (squash, AI authored)")
        .unwrap();

    // Sanity check: the squash commit is AI-attributed.
    let mut squash_file = repo.filename("feature.ts");
    squash_file.assert_committed_lines(crate::lines![
        "export const feature = () => \"written by AI\";".ai(),
    ]);

    let squash_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // --- Phase B: split into a fresh stack branch off the base via restore ---
    // New branch from the base (NOT from the squash branch), then pull the file
    // content out of the squash commit and commit it. No checkpoint fires.
    repo.git(&["switch", "-c", "stack", &main_branch]).unwrap();
    repo.git(&[
        "restore",
        "--source",
        &squash_commit,
        "--staged",
        "--worktree",
        "--",
        "feature.ts",
    ])
    .unwrap();
    repo.stage_all_and_commit("Add feature (stack)").unwrap();

    // The restored content is byte-identical to the AI-authored squash commit and
    // must remain AI-attributed.
    let mut stack_file = repo.filename("feature.ts");
    stack_file.assert_committed_lines(crate::lines![
        "export const feature = () => \"written by AI\";".ai(),
    ]);

    let stats = repo.stats().unwrap();
    assert_eq!(
        stats.ai_additions, 1,
        "restored AI content should count as 1 AI addition, got stats: {stats:?}"
    );
    assert_eq!(
        stats.unknown_additions, 0,
        "restored AI content must not be untracked, got stats: {stats:?}"
    );
}

/// Human-authored content restored via `--source` should stay human, never be
/// misattributed to AI.
#[test]
fn test_restore_source_preserves_human_attribution() {
    let repo = TestRepo::new();

    let base_path = repo.path().join("base.txt");
    fs::write(&base_path, "base line\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let main_branch = repo.current_branch();

    repo.git(&["switch", "-c", "squash", &main_branch]).unwrap();
    let human_path = repo.path().join("human.ts");
    fs::write(
        &human_path,
        "export const human = () => \"typed by hand\";\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "human.ts"])
        .unwrap();
    repo.stage_all_and_commit("Add human file (squash)")
        .unwrap();

    let mut squash_file = repo.filename("human.ts");
    squash_file.assert_committed_lines(crate::lines![
        "export const human = () => \"typed by hand\";".human(),
    ]);
    let squash_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.git(&["switch", "-c", "stack", &main_branch]).unwrap();
    repo.git(&[
        "restore",
        "--source",
        &squash_commit,
        "--staged",
        "--worktree",
        "--",
        "human.ts",
    ])
    .unwrap();
    repo.stage_all_and_commit("Add human file (stack)").unwrap();

    let mut stack_file = repo.filename("human.ts");
    stack_file.assert_committed_lines(crate::lines![
        "export const human = () => \"typed by hand\";".human(),
    ]);

    let stats = repo.stats().unwrap();
    assert_eq!(
        stats.ai_additions, 0,
        "human-source restore must not produce AI attribution, got stats: {stats:?}"
    );
}

/// A partial restore (subset of the source's files) attributes only the
/// restored files; unrestored files do not leak in.
#[test]
fn test_restore_source_partial_subset_only_attributes_restored_files() {
    let repo = TestRepo::new();

    let base_path = repo.path().join("base.txt");
    fs::write(&base_path, "base line\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let main_branch = repo.current_branch();

    repo.git(&["switch", "-c", "squash", &main_branch]).unwrap();
    let a_path = repo.path().join("a.ts");
    let b_path = repo.path().join("b.ts");
    fs::write(&a_path, "export const a = () => \"ai a\";\n").unwrap();
    fs::write(&b_path, "export const b = () => \"ai b\";\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "a.ts"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "b.ts"]).unwrap();
    repo.stage_all_and_commit("Add a and b (squash)").unwrap();
    let squash_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Restore only a.ts onto a fresh branch.
    repo.git(&["switch", "-c", "stack", &main_branch]).unwrap();
    repo.git(&[
        "restore",
        "--source",
        &squash_commit,
        "--staged",
        "--worktree",
        "--",
        "a.ts",
    ])
    .unwrap();
    repo.stage_all_and_commit("Add a (stack)").unwrap();

    let mut a_file = repo.filename("a.ts");
    a_file.assert_committed_lines(crate::lines!["export const a = () => \"ai a\";".ai()]);

    // b.ts was never restored onto this branch, so it must not exist here.
    assert!(
        !repo.path().join("b.ts").exists(),
        "b.ts should not be present on the stack branch"
    );

    let stats = repo.stats().unwrap();
    assert_eq!(
        stats.ai_additions, 1,
        "only the restored file should be AI-attributed, got stats: {stats:?}"
    );
}

/// `--source` accepts a relative revision (e.g. HEAD~1); it must resolve and
/// attribute correctly.
#[test]
fn test_restore_source_relative_ref_resolves() {
    let repo = TestRepo::new();

    let base_path = repo.path().join("base.txt");
    fs::write(&base_path, "base line\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let main_branch = repo.current_branch();

    // Squash branch: AI file committed, then a follow-up commit on top, so that
    // from this branch HEAD~1 points at the AI commit.
    repo.git(&["switch", "-c", "squash", &main_branch]).unwrap();
    let feature_path = repo.path().join("feature.ts");
    fs::write(&feature_path, "export const feature = () => \"ai\";\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "feature.ts"])
        .unwrap();
    repo.stage_all_and_commit("Add AI feature").unwrap();

    let other_path = repo.path().join("other.txt");
    fs::write(&other_path, "unrelated\n").unwrap();
    repo.stage_all_and_commit("Unrelated commit").unwrap();

    // Fresh branch off the base (without feature.ts), restore it from the
    // squash branch's HEAD~1 (a relative ref that must resolve), then commit.
    repo.git(&["switch", "-c", "stack", &main_branch]).unwrap();
    repo.git(&[
        "restore",
        "--source",
        "squash~1",
        "--staged",
        "--worktree",
        "--",
        "feature.ts",
    ])
    .unwrap();
    repo.stage_all_and_commit("Add feature from squash~1")
        .unwrap();

    let mut feature_file = repo.filename("feature.ts");
    feature_file.assert_committed_lines(crate::lines!["export const feature = () => \"ai\";".ai()]);
}

/// A plain `git restore <file>` (no `--source`) restores from the index and is
/// not an attribution-bearing cross-commit move; it must not corrupt existing
/// attribution or fabricate AI attribution.
#[test]
fn test_restore_without_source_is_noop_for_attribution() {
    let repo = TestRepo::new();

    let base_path = repo.path().join("base.txt");
    fs::write(&base_path, "base line\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI file, checkpointed and committed.
    let feature_path = repo.path().join("feature.ts");
    fs::write(&feature_path, "export const feature = () => \"ai\";\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "feature.ts"])
        .unwrap();
    repo.stage_all_and_commit("Add AI feature").unwrap();

    // Make an uncommitted local edit, then discard it with a no-source restore.
    fs::write(
        &feature_path,
        "export const feature = () => \"local edit\";\n",
    )
    .unwrap();
    repo.git(&["restore", "--", "feature.ts"]).unwrap();

    // The committed attribution is unchanged and no spurious commit was created.
    let mut feature_file = repo.filename("feature.ts");
    feature_file.assert_committed_lines(crate::lines!["export const feature = () => \"ai\";".ai()]);
}
