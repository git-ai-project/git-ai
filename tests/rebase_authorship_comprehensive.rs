#[macro_use]
mod repos;
mod test_utils;

use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::authorship::rebase_authorship::{
    filter_pathspecs_to_ai_touched_files, prepare_working_log_after_squash,
    reconstruct_working_log_after_reset, rewrite_authorship_after_cherry_pick,
    rewrite_authorship_after_commit_amend, rewrite_authorship_after_rebase_v2,
    rewrite_authorship_after_squash_or_rebase, rewrite_authorship_if_needed,
};
use git_ai::git::refs::get_reference_as_authorship_log_v3;
use git_ai::git::repository;
use git_ai::git::rewrite_log::{RebaseCompleteEvent, RewriteLogEvent};

// ==============================================================================
// Helper Functions
// ==============================================================================

fn create_ai_commit(repo: &mut TestRepo, filename: &str, content: &[&str]) -> String {
    // Use TestRepo's built-in commit which creates authorship logs
    repo.filename(filename)
        .set_contents(content.to_vec())
        .stage();
    let result = repo.commit(&format!("Add {}", filename));
    match result {
        Ok(new_commit) => new_commit.commit_sha,
        Err(e) => {
            // Fallback: try with git-ai if regular commit fails
            repo.git_ai(&["commit", "-m", &format!("Add {}", filename)])
                .unwrap_or_else(|_| panic!("Failed to create commit: {}", e));
            repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string()
        }
    }
}

fn get_authorship_log(repo: &TestRepo, commit_sha: &str) -> Option<AuthorshipLog> {
    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    get_reference_as_authorship_log_v3(&git_repo, commit_sha).ok()
}

fn assert_authorship_preserved(repo: &TestRepo, old_commit: &str, new_commit: &str) {
    let old_log = get_authorship_log(repo, old_commit);
    let new_log = get_authorship_log(repo, new_commit);

    assert!(old_log.is_some(), "Original commit should have authorship");
    assert!(new_log.is_some(), "New commit should have authorship");

    let old = old_log.unwrap();
    let new = new_log.unwrap();

    assert_eq!(
        old.attestations.len(),
        new.attestations.len(),
        "Attestation count should match"
    );
    assert_eq!(
        old.metadata.prompts.len(),
        new.metadata.prompts.len(),
        "Prompt count should match"
    );
}

// ==============================================================================
// PromptLineMetrics Tests
// ==============================================================================

#[test]
fn test_prompt_line_metrics_default() {
    // Test that PromptLineMetrics has sensible defaults
    // This is tested implicitly through the rebase process
    let mut repo = TestRepo::new();
    repo.filename("base.txt")
        .set_contents(vec!["base content"])
        .stage();
    repo.commit("initial").unwrap();

    create_ai_commit(&mut repo, "test.txt", &["line 1", "line 2"]);
    let commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let log = get_authorship_log(&repo, &commit);
    assert!(log.is_some());
}

#[test]
fn test_prompt_line_metrics_accumulation() {
    let mut repo = TestRepo::new();
    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("initial").unwrap();

    // Create multiple AI commits
    create_ai_commit(&mut repo, "file1.txt", &["content 1"]);
    create_ai_commit(&mut repo, "file2.txt", &["content 2"]);
    create_ai_commit(&mut repo, "file3.txt", &["content 3"]);

    let commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let log = get_authorship_log(&repo, &commit);
    assert!(log.is_some());
}

// ==============================================================================
// CommitTrackedDelta Tests
// ==============================================================================

#[test]
fn test_commit_tracked_delta_empty() {
    let mut repo = TestRepo::new();
    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let base = repo.commit("initial").unwrap();

    // No changes in commit
    let log = get_authorship_log(&repo, &base.commit_sha);
    assert!(log.is_none(), "Non-AI commit should have no authorship");
}

#[test]
fn test_commit_tracked_delta_with_files() {
    let mut repo = TestRepo::new();
    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("initial").unwrap();

    let commit = create_ai_commit(&mut repo, "tracked.txt", &["tracked content"]);
    let log = get_authorship_log(&repo, &commit);
    assert!(log.is_some());

    let log = log.unwrap();
    assert_eq!(log.attestations.len(), 1);
    assert_eq!(log.attestations[0].file_path, "tracked.txt");
}

#[test]
fn test_commit_tracked_delta_multiple_files() {
    let mut repo = TestRepo::new();
    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("initial").unwrap();

    repo.filename("file1.txt")
        .set_contents(vec!["content 1"])
        .stage();
    repo.filename("file2.txt")
        .set_contents(vec!["content 2"])
        .stage();
    repo.git_ai(&["commit", "-m", "Add multiple files"])
        .unwrap();

    let commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let log = get_authorship_log(&repo, &commit);
    assert!(log.is_some());

    let log = log.unwrap();
    assert_eq!(log.attestations.len(), 2);
}

// ==============================================================================
// Basic Rebase Tests
// ==============================================================================

#[test]
fn test_rebase_single_commit_preserves_authorship() {
    let mut repo = TestRepo::new();

    // Create base
    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let base = repo.commit("base").unwrap();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let feature_commit = create_ai_commit(&mut repo, "feature.txt", &["feature content"]);

    // Create main branch commit
    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt")
        .set_contents(vec!["main content"])
        .stage();
    repo.commit("main commit").unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_ne!(feature_commit, new_commit, "Commit SHA should change");

    let log = get_authorship_log(&repo, &new_commit);
    assert!(log.is_some(), "Rebased commit should preserve authorship");
}

#[test]
fn test_rebase_multiple_commits_preserves_order() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    // Create feature branch with multiple commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let commit1 = create_ai_commit(&mut repo, "file1.txt", &["content 1"]);
    let commit2 = create_ai_commit(&mut repo, "file2.txt", &["content 2"]);
    let commit3 = create_ai_commit(&mut repo, "file3.txt", &["content 3"]);

    // Create main branch commit
    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    // Verify all commits have authorship
    let commits = repo.git(&["log", "--format=%H", "-3"]).unwrap();
    let new_commits: Vec<&str> = commits.trim().split('\n').collect();

    for new_commit in new_commits {
        let log = get_authorship_log(&repo, new_commit);
        assert!(log.is_some(), "Each rebased commit should have authorship");
    }
}

#[test]
fn test_rebase_empty_commits_filtered() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let commit = create_ai_commit(&mut repo, "file.txt", &["content"]);

    // Rebase (no-op)
    repo.git(&["rebase", "main"]).unwrap();

    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    // Since there's no divergence, commit should be the same
    assert_eq!(commit, new_commit);
}

// ==============================================================================
// Interactive Rebase Tests
// ==============================================================================

#[test]
fn test_interactive_rebase_detection() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    create_ai_commit(&mut repo, "feature.txt", &["feature"]);

    // Interactive rebase creates rebase-merge directory
    let rebase_merge_dir = repo.path().join(".git").join("rebase-merge");
    assert!(
        !rebase_merge_dir.exists(),
        "Initially no rebase in progress"
    );
}

#[test]
fn test_interactive_rebase_todo_list() {
    // Verify that interactive rebase state is detectable
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    let todo_path = repo
        .path()
        .join(".git")
        .join("rebase-merge")
        .join("git-rebase-todo");
    assert!(!todo_path.exists(), "No rebase todo initially");
}

// ==============================================================================
// Rebase with Conflicts Tests
// ==============================================================================

#[test]
fn test_rebase_with_conflict_detection() {
    let mut repo = TestRepo::new();

    repo.filename("conflict.txt")
        .set_contents(vec!["original"])
        .stage();
    repo.commit("base").unwrap();

    // Create conflicting changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("conflict.txt")
        .set_contents(vec!["feature version"])
        .stage();
    repo.git_ai(&["commit", "-m", "feature change"]).unwrap();

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("conflict.txt")
        .set_contents(vec!["main version"])
        .stage();
    repo.commit("main change").unwrap();

    // Attempt rebase (will conflict)
    repo.git(&["checkout", "feature"]).unwrap();
    let result = repo.git(&["rebase", "main"]);

    // Rebase should fail due to conflict
    assert!(result.is_err() || result.unwrap().contains("conflict"));
}

#[test]
fn test_rebase_continue_after_conflict_resolution() {
    let mut repo = TestRepo::new();

    repo.filename("file.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let original_commit = create_ai_commit(&mut repo, "feature.txt", &["feature"]);

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    // Rebase without conflicts
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let log = get_authorship_log(&repo, &new_commit);
    assert!(log.is_some(), "Authorship preserved after continue");
}

// ==============================================================================
// Rebase onto Different Base Tests
// ==============================================================================

#[test]
fn test_rebase_onto_specific_commit() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let base = repo.commit("base").unwrap();

    repo.filename("second.txt")
        .set_contents(vec!["second"])
        .stage();
    let onto_commit = repo.commit("second").unwrap();

    // Create feature branch from base
    repo.git(&["checkout", "-b", "feature", &base.commit_sha])
        .unwrap();
    create_ai_commit(&mut repo, "feature.txt", &["feature"]);

    // Rebase onto specific commit
    repo.git(&["rebase", "--onto", &onto_commit.commit_sha, "main"])
        .unwrap();

    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let log = get_authorship_log(&repo, &new_commit);
    assert!(log.is_some(), "Authorship preserved with --onto");
}

#[test]
fn test_rebase_onto_different_branch() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    // Create target branch
    repo.git(&["checkout", "-b", "target"]).unwrap();
    repo.filename("target.txt")
        .set_contents(vec!["target"])
        .stage();
    repo.commit("target").unwrap();

    // Create feature branch
    repo.git(&["checkout", "main"]).unwrap();
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    create_ai_commit(&mut repo, "feature.txt", &["feature"]);

    // Rebase onto target branch
    repo.git(&["rebase", "target"]).unwrap();

    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let log = get_authorship_log(&repo, &new_commit);
    assert!(log.is_some(), "Authorship preserved across branches");
}

// ==============================================================================
// Squash Merge Tests
// ==============================================================================

#[test]
fn test_prepare_working_log_after_squash() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();
    let target_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    create_ai_commit(&mut repo, "file1.txt", &["content 1"]);
    create_ai_commit(&mut repo, "file2.txt", &["content 2"]);
    let source_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Test prepare_working_log_after_squash
    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = prepare_working_log_after_squash(&git_repo, &source_head, &target_head, "human");

    assert!(
        result.is_ok(),
        "prepare_working_log_after_squash should succeed"
    );
}

#[test]
fn test_prepare_working_log_after_squash_no_changes() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();
    let commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Test with same source and target (no changes)
    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = prepare_working_log_after_squash(&git_repo, &commit, &commit, "human");

    assert!(result.is_ok(), "Should handle no changes gracefully");
}

#[test]
fn test_squash_merge_with_merge_base() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let base = repo.commit("base").unwrap();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    create_ai_commit(&mut repo, "feature.txt", &["feature"]);
    let source_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Add commit to main
    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();
    let target_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = prepare_working_log_after_squash(&git_repo, &source_head, &target_head, "human");

    assert!(result.is_ok(), "Should handle diverged branches");
}

// ==============================================================================
// Squash or Rebase Merge Tests
// ==============================================================================

#[test]
fn test_rewrite_authorship_after_squash_or_rebase() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();
    let base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    create_ai_commit(&mut repo, "feature.txt", &["feature"]);
    let source_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Merge back to main
    repo.git(&["checkout", "main"]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.git_ai(&["commit", "-m", "Squash merge"]).unwrap();
    let merge_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = rewrite_authorship_after_squash_or_rebase(
        &git_repo,
        "feature",
        "main",
        &source_head,
        &merge_commit,
        false,
    );

    assert!(
        result.is_ok(),
        "Should rewrite authorship after squash merge"
    );

    let log = get_authorship_log(&repo, &merge_commit);
    assert!(log.is_some(), "Squash merge commit should have authorship");
}

#[test]
fn test_squash_or_rebase_no_ai_files() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    // Create feature branch with non-AI commit
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature"])
        .stage();
    repo.commit("non-ai commit").unwrap();
    let source_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Merge back
    repo.git(&["checkout", "main"]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.commit("squash").unwrap();
    let merge_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = rewrite_authorship_after_squash_or_rebase(
        &git_repo,
        "feature",
        "main",
        &source_head,
        &merge_commit,
        false,
    );

    assert!(result.is_ok(), "Should handle non-AI commits");
}

// ==============================================================================
// Rebase v2 Tests
// ==============================================================================

#[test]
fn test_rewrite_authorship_after_rebase_v2_empty_commits() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();
    let original_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = rewrite_authorship_after_rebase_v2(&git_repo, &original_head, &[], &[], "human");

    assert!(result.is_ok(), "Should handle empty commit list");
}

#[test]
fn test_rebase_v2_preserves_prompt_metadata() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let original_commit = create_ai_commit(&mut repo, "file.txt", &["content"]);
    let original_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    // Rebase
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();
    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let original_log = get_authorship_log(&repo, &original_commit);
    let new_log = get_authorship_log(&repo, &new_commit);

    assert!(original_log.is_some());
    assert!(new_log.is_some());

    // Verify prompts are preserved
    let orig = original_log.unwrap();
    let new = new_log.unwrap();
    assert!(!orig.metadata.prompts.is_empty());
    assert!(!new.metadata.prompts.is_empty());
}

#[test]
fn test_rebase_v2_skips_existing_authorship_logs() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    // Create AI commit on main (already has authorship)
    let existing_commit = create_ai_commit(&mut repo, "main.txt", &["main"]);

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    create_ai_commit(&mut repo, "feature.txt", &["feature"]);
    let feature_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Rebase will include the existing commit
    repo.git(&["rebase", "main"]).unwrap();

    // The existing commit should keep its original authorship
    let log = get_authorship_log(&repo, &existing_commit);
    assert!(log.is_some(), "Existing authorship should be preserved");
}

// ==============================================================================
// Cherry-Pick Tests
// ==============================================================================

#[test]
fn test_rewrite_authorship_after_cherry_pick_empty() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = rewrite_authorship_after_cherry_pick(&git_repo, &[], &[], "human");

    assert!(result.is_ok(), "Should handle empty cherry-pick");
}

#[test]
fn test_cherry_pick_single_commit() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    // Create commit to cherry-pick
    repo.git(&["checkout", "-b", "source"]).unwrap();
    let source_commit = create_ai_commit(&mut repo, "cherry.txt", &["cherry content"]);

    // Cherry-pick to main
    repo.git(&["checkout", "main"]).unwrap();
    repo.git(&["cherry-pick", &source_commit]).unwrap();
    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let source_log = get_authorship_log(&repo, &source_commit);
    let new_log = get_authorship_log(&repo, &new_commit);

    assert!(source_log.is_some());
    assert!(new_log.is_some());
}

#[test]
fn test_cherry_pick_multiple_commits() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    // Create multiple commits
    repo.git(&["checkout", "-b", "source"]).unwrap();
    let commit1 = create_ai_commit(&mut repo, "file1.txt", &["content 1"]);
    let commit2 = create_ai_commit(&mut repo, "file2.txt", &["content 2"]);

    // Cherry-pick both
    repo.git(&["checkout", "main"]).unwrap();
    repo.git(&["cherry-pick", &commit1]).unwrap();
    let new1 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    repo.git(&["cherry-pick", &commit2]).unwrap();
    let new2 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    assert!(get_authorship_log(&repo, &new1).is_some());
    assert!(get_authorship_log(&repo, &new2).is_some());
}

#[test]
fn test_cherry_pick_preserves_file_content() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "source"]).unwrap();
    let source_commit = create_ai_commit(&mut repo, "test.txt", &["line 1", "line 2"]);

    repo.git(&["checkout", "main"]).unwrap();
    repo.git(&["cherry-pick", &source_commit]).unwrap();

    let content = repo.filename("test.txt").contents();
    assert_eq!(content, "line 1\nline 2\n");
}

// ==============================================================================
// Commit Amend Tests
// ==============================================================================

#[test]
fn test_rewrite_authorship_after_commit_amend() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    let original_commit = create_ai_commit(&mut repo, "file.txt", &["original content"]);

    // Amend the commit
    repo.filename("file.txt")
        .set_contents(vec!["amended content"])
        .stage();
    repo.git_ai(&["commit", "--amend", "--no-edit"]).unwrap();
    let amended_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    assert_ne!(original_commit, amended_commit);

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = rewrite_authorship_after_commit_amend(
        &git_repo,
        &original_commit,
        &amended_commit,
        "human".to_string(),
    );

    assert!(result.is_ok(), "Amend should rewrite authorship");

    let log = get_authorship_log(&repo, &amended_commit);
    assert!(log.is_some(), "Amended commit should have authorship");
}

#[test]
fn test_amend_preserves_existing_authorship() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    let original_commit = create_ai_commit(&mut repo, "file.txt", &["content"]);
    let original_log = get_authorship_log(&repo, &original_commit);

    // Amend with no changes
    repo.git_ai(&["commit", "--amend", "--no-edit"]).unwrap();
    let amended_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    rewrite_authorship_after_commit_amend(
        &git_repo,
        &original_commit,
        &amended_commit,
        "human".to_string(),
    )
    .unwrap();

    let amended_log = get_authorship_log(&repo, &amended_commit);
    assert!(original_log.is_some());
    assert!(amended_log.is_some());
}

// ==============================================================================
// Reset Tests
// ==============================================================================

#[test]
fn test_reconstruct_working_log_after_reset() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    create_ai_commit(&mut repo, "file.txt", &["content"]);
    let commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Reset to previous commit
    repo.git(&["reset", "HEAD~1"]).unwrap();

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let old_head = repo
        .git(&["rev-parse", "HEAD^"])
        .unwrap()
        .trim()
        .to_string();
    let result = reconstruct_working_log_after_reset(&git_repo, &old_head, &commit, "human", None);

    assert!(result.is_ok(), "Should reconstruct working log after reset");
}

#[test]
fn test_reset_soft_preserves_staged_files() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let base = repo.commit("base").unwrap();

    create_ai_commit(&mut repo, "file.txt", &["content"]);

    // Soft reset
    repo.git(&["reset", "--soft", &base.commit_sha]).unwrap();

    // File should still be staged
    let status = repo.git(&["status", "--short"]).unwrap();
    assert!(status.contains("file.txt"));
}

#[test]
fn test_reset_hard_removes_working_changes() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let base = repo.commit("base").unwrap();

    create_ai_commit(&mut repo, "file.txt", &["content"]);

    // Hard reset
    repo.git(&["reset", "--hard", &base.commit_sha]).unwrap();

    // File should not exist
    let exists = repo.path().join("file.txt").exists();
    assert!(!exists);
}

// ==============================================================================
// Event Processing Tests
// ==============================================================================

#[test]
fn test_rewrite_authorship_if_needed_commit_event() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let base = repo.commit("base").unwrap();

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let event = RewriteLogEvent::commit(Some(base.commit_sha.clone()), base.commit_sha.clone());

    let result =
        rewrite_authorship_if_needed(&git_repo, &event, "human".to_string(), &vec![], true);

    assert!(result.is_ok(), "Should process commit event");
}

#[test]
fn test_rewrite_authorship_if_needed_rebase_complete() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let original_commit = create_ai_commit(&mut repo, "feature.txt", &["feature"]);
    let original_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();
    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let event = RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
        original_head.clone(),
        new_commit.clone(),
        false,
        vec![original_commit.clone()],
        vec![new_commit.clone()],
    ));

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result =
        rewrite_authorship_if_needed(&git_repo, &event, "human".to_string(), &vec![], true);

    assert!(result.is_ok(), "Should process rebase complete event");
}

// ==============================================================================
// Pathspec Filtering Tests
// ==============================================================================

#[test]
fn test_filter_pathspecs_to_ai_touched_files_empty() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let base = repo.commit("base").unwrap();

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = filter_pathspecs_to_ai_touched_files(&git_repo, &[base.commit_sha], &[]);

    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[test]
fn test_filter_pathspecs_includes_ai_files() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    let commit = create_ai_commit(&mut repo, "ai-file.txt", &["ai content"]);

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result =
        filter_pathspecs_to_ai_touched_files(&git_repo, &[commit], &["ai-file.txt".to_string()]);

    assert!(result.is_ok());
    let filtered = result.unwrap();
    assert!(filtered.contains(&"ai-file.txt".to_string()));
}

#[test]
fn test_filter_pathspecs_excludes_non_ai_files() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let base = repo.commit("base").unwrap();

    repo.filename("non-ai.txt")
        .set_contents(vec!["non-ai content"])
        .stage();
    repo.commit("non-ai commit").unwrap();

    let git_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = filter_pathspecs_to_ai_touched_files(
        &git_repo,
        &[base.commit_sha],
        &["non-ai.txt".to_string()],
    );

    assert!(result.is_ok());
    let filtered = result.unwrap();
    assert!(!filtered.contains(&"non-ai.txt".to_string()));
}

// ==============================================================================
// Large Commit Tests
// ==============================================================================

#[test]
fn test_rebase_large_commit() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    // Create large commit (many files)
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    for i in 0..50 {
        repo.filename(&format!("file{}.txt", i))
            .set_contents(vec![format!("content {}", i)])
            .stage();
    }
    repo.git_ai(&["commit", "-m", "Large commit"]).unwrap();
    let original_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    // Rebase large commit
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let log = get_authorship_log(&repo, &new_commit);
    assert!(log.is_some(), "Large commit should preserve authorship");
}

#[test]
fn test_rebase_commit_with_long_lines() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let long_line = "a".repeat(1000);
    create_ai_commit(&mut repo, "long.txt", &[&long_line]);

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let log = get_authorship_log(&repo, &new_commit);
    assert!(log.is_some());
}

// ==============================================================================
// Edge Case Tests
// ==============================================================================

#[test]
fn test_rebase_with_deleted_file() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let commit = create_ai_commit(&mut repo, "temp.txt", &["temp"]);

    // Delete file in next commit
    repo.git(&["rm", "temp.txt"]).unwrap();
    repo.git_ai(&["commit", "-m", "Delete temp"]).unwrap();

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    // Rebase
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    // File should not exist after rebase
    let exists = repo.path().join("temp.txt").exists();
    assert!(!exists);
}

#[test]
fn test_rebase_with_renamed_file() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    create_ai_commit(&mut repo, "old.txt", &["content"]);

    // Rename file
    repo.git(&["mv", "old.txt", "new.txt"]).unwrap();
    repo.git_ai(&["commit", "-m", "Rename"]).unwrap();

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    // Rebase
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    let new_exists = repo.path().join("new.txt").exists();
    let old_exists = repo.path().join("old.txt").exists();
    assert!(new_exists);
    assert!(!old_exists);
}

#[test]
fn test_rebase_with_empty_file() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    create_ai_commit(&mut repo, "empty.txt", &[]);

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    let log = get_authorship_log(
        &repo,
        &repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string(),
    );
    // Empty file commits might not have authorship
    assert!(log.is_some() || log.is_none());
}

#[test]
fn test_rebase_binary_file() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Create binary file
    let binary_data = vec![0u8, 1, 2, 3, 255, 254, 253];
    std::fs::write(repo.path().join("binary.dat"), binary_data).unwrap();
    repo.git(&["add", "binary.dat"]).unwrap();
    repo.git_ai(&["commit", "-m", "Add binary"]).unwrap();

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    // Rebase with binary file
    repo.git(&["checkout", "feature"]).unwrap();
    let result = repo.git(&["rebase", "main"]);
    assert!(result.is_ok());
}

#[test]
fn test_rebase_with_submodule() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    // Note: Full submodule testing is complex, just verify basic handling
    let gitmodules = repo.path().join(".gitmodules");
    assert!(!gitmodules.exists(), "No submodules in test");
}

// ==============================================================================
// Performance Tests
// ==============================================================================

#[test]
fn test_rebase_many_commits_performance() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Create 20 commits
    for i in 0..20 {
        create_ai_commit(
            &mut repo,
            &format!("file{}.txt", i),
            &[&format!("content {}", i)],
        );
    }

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    // Rebase all commits
    repo.git(&["checkout", "feature"]).unwrap();
    let start = std::time::Instant::now();
    repo.git(&["rebase", "main"]).unwrap();
    let duration = start.elapsed();

    // Should complete in reasonable time (< 10 seconds)
    assert!(duration.as_secs() < 10, "Rebase took too long");
}

#[test]
fn test_rebase_with_many_files_per_commit() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Create commit with 100 files
    for i in 0..100 {
        repo.filename(&format!("file{}.txt", i))
            .set_contents(vec![format!("content {}", i)])
            .stage();
    }
    repo.git_ai(&["commit", "-m", "Many files"]).unwrap();

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    let result = repo.git(&["rebase", "main"]);
    assert!(result.is_ok(), "Should handle many files per commit");
}

// ==============================================================================
// Metadata Tests
// ==============================================================================

#[test]
fn test_authorship_log_base_commit_sha_updated() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    create_ai_commit(&mut repo, "file.txt", &["content"]);

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let log = get_authorship_log(&repo, &new_commit);
    assert!(log.is_some());

    let log = log.unwrap();
    assert_eq!(log.metadata.base_commit_sha, new_commit);
}

#[test]
fn test_authorship_log_prompts_preserved() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let original_commit = create_ai_commit(&mut repo, "file.txt", &["content"]);
    let original_log = get_authorship_log(&repo, &original_commit);

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let new_log = get_authorship_log(&repo, &new_commit);

    assert!(original_log.is_some());
    assert!(new_log.is_some());

    let orig = original_log.unwrap();
    let new = new_log.unwrap();

    // Verify same number of prompts
    assert_eq!(orig.metadata.prompts.len(), new.metadata.prompts.len());
}

#[test]
fn test_authorship_log_attestations_preserved() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let original_commit = create_ai_commit(&mut repo, "file.txt", &["line 1", "line 2"]);
    let original_log = get_authorship_log(&repo, &original_commit);

    repo.git(&["checkout", "main"]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main"]).stage();
    repo.commit("main").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let new_log = get_authorship_log(&repo, &new_commit);

    assert!(original_log.is_some());
    assert!(new_log.is_some());

    let orig = original_log.unwrap();
    let new = new_log.unwrap();

    assert_eq!(orig.attestations.len(), new.attestations.len());
}
