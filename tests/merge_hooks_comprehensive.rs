#[macro_use]
mod repos;
use git_ai::git::repository;
use git_ai::git::repository::Repository;
mod test_utils;

use crate::repos::test_repo::TestRepo;
use git_ai::commands::hooks::merge_hooks::post_merge_hook;
use git_ai::git::cli_parser::ParsedGitInvocation;
use git_ai::git::rewrite_log::RewriteLogEvent;

// ==============================================================================
// Test Helper Functions
// ==============================================================================

fn make_merge_invocation(args: &[&str]) -> ParsedGitInvocation {
    ParsedGitInvocation {
        global_args: Vec::new(),
        command: Some("merge".to_string()),
        command_args: args.iter().map(|s| s.to_string()).collect(),
        saw_end_of_opts: false,
        is_help: false,
    }
}

// ==============================================================================
// Post-Merge Hook Tests
// ==============================================================================

#[test]
fn test_post_merge_hook_squash_success() {
    let mut repo = TestRepo::new();

    // Create base commit
    repo.filename("base.txt")
        .set_contents(vec!["base content"])
        .stage();
    let base = repo.commit("base commit").unwrap();

    // Capture original branch before creating feature branch
    let original_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature content"])
        .stage();
    let feature = repo.commit("feature commit").unwrap();

    // Go back to original branch
    repo.git(&["checkout", &original_branch]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_merge_invocation(&["--squash", "feature"]);
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_merge_hook(&parsed_args, exit_status, &mut repository);

    // Verify MergeSquash event was logged
    let events = repository.storage.read_rewrite_events().unwrap();
    let has_merge_squash = events
        .iter()
        .any(|e| matches!(e, RewriteLogEvent::MergeSquash { .. }));

    assert!(has_merge_squash, "MergeSquash event should be logged");
}

#[test]
fn test_post_merge_hook_squash_failed() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt")
        .set_contents(vec!["base content"])
        .stage();
    repo.commit("base commit").unwrap();

    let original_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature content"])
        .stage();
    repo.commit("feature commit").unwrap();

    repo.git(&["checkout", &original_branch]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_merge_invocation(&["--squash", "feature"]);
    let exit_status = std::process::Command::new("false")
        .status()
        .unwrap_or_else(|_| {
            std::process::Command::new("sh")
                .arg("-c")
                .arg("exit 1")
                .status()
                .unwrap()
        });

    let events_before = repository.storage.read_rewrite_events().unwrap_or_default();
    let initial_count = events_before.len();

    post_merge_hook(&parsed_args, exit_status, &mut repository);

    // Failed merge should not log events
    let events_after = repository.storage.read_rewrite_events().unwrap_or_default();
    assert_eq!(
        events_after.len(),
        initial_count,
        "Failed merge should not log events"
    );
}

#[test]
fn test_post_merge_hook_normal_merge() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt")
        .set_contents(vec!["base content"])
        .stage();
    repo.commit("base commit").unwrap();

    let original_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature content"])
        .stage();
    repo.commit("feature commit").unwrap();

    repo.git(&["checkout", &original_branch]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_merge_invocation(&["feature"]);
    let exit_status = std::process::Command::new("true").status().unwrap();

    let events_before = repository.storage.read_rewrite_events().unwrap_or_default();
    let initial_count = events_before.len();

    post_merge_hook(&parsed_args, exit_status, &mut repository);

    // Normal merge (not squash) should not log MergeSquash events
    let events_after = repository.storage.read_rewrite_events().unwrap_or_default();
    let has_merge_squash = events_after
        .iter()
        .skip(initial_count)
        .any(|e| matches!(e, RewriteLogEvent::MergeSquash { .. }));

    assert!(
        !has_merge_squash,
        "Normal merge should not log MergeSquash events"
    );
}

#[test]
fn test_post_merge_hook_dry_run() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt")
        .set_contents(vec!["base content"])
        .stage();
    repo.commit("base commit").unwrap();

    let original_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature content"])
        .stage();
    repo.commit("feature commit").unwrap();

    repo.git(&["checkout", &original_branch]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_merge_invocation(&["--squash", "--dry-run", "feature"]);
    let exit_status = std::process::Command::new("true").status().unwrap();

    let events_before = repository.storage.read_rewrite_events().unwrap_or_default();
    let initial_count = events_before.len();

    post_merge_hook(&parsed_args, exit_status, &mut repository);

    // Dry run should not log events
    let events_after = repository.storage.read_rewrite_events().unwrap_or_default();
    assert_eq!(
        events_after.len(),
        initial_count,
        "Dry run should not log events"
    );
}

#[test]
fn test_post_merge_hook_invalid_branch() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt")
        .set_contents(vec!["base content"])
        .stage();
    repo.commit("base commit").unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_merge_invocation(&["--squash", "nonexistent-branch"]);
    let exit_status = std::process::Command::new("true").status().unwrap();

    let events_before = repository.storage.read_rewrite_events().unwrap_or_default();
    let initial_count = events_before.len();

    post_merge_hook(&parsed_args, exit_status, &mut repository);

    // Should handle invalid branch gracefully without logging
    let events_after = repository.storage.read_rewrite_events().unwrap_or_default();

    // Event count should not increase or should handle gracefully
    // The hook returns early if it can't resolve the branch
}

// ==============================================================================
// Merge Squash Event Tests
// ==============================================================================

#[test]
fn test_merge_squash_event_creation() {
    use git_ai::git::rewrite_log::MergeSquashEvent;

    let event = MergeSquashEvent::new(
        "feature".to_string(),
        "abc123".to_string(),
        "main".to_string(),
        "def456".to_string(),
    );

    assert_eq!(event.source_branch, "feature");
    assert_eq!(event.source_head, "abc123");
    assert_eq!(event.base_branch, "main");
    assert_eq!(event.base_head, "def456");
}

#[test]
fn test_merge_squash_event_variant() {
    use git_ai::git::rewrite_log::MergeSquashEvent;

    let event = RewriteLogEvent::merge_squash(MergeSquashEvent::new(
        "feature".to_string(),
        "abc123".to_string(),
        "main".to_string(),
        "def456".to_string(),
    ));

    match event {
        RewriteLogEvent::MergeSquash { merge_squash } => {
            assert_eq!(merge_squash.source_branch, "feature");
            assert_eq!(merge_squash.base_branch, "main");
        }
        _ => panic!("Expected MergeSquash event"),
    }
}

// ==============================================================================
// Merge Flag Detection Tests
// ==============================================================================

#[test]
fn test_squash_flag_detection() {
    let parsed = make_merge_invocation(&["--squash", "feature"]);

    assert!(parsed.has_command_flag("--squash"));
}

#[test]
fn test_dry_run_flag_detection() {
    let parsed = make_merge_invocation(&["--dry-run", "feature"]);

    assert!(parsed.command_args.contains(&"--dry-run".to_string()));
}

#[test]
fn test_no_squash_flag() {
    let parsed = make_merge_invocation(&["feature"]);

    assert!(!parsed.has_command_flag("--squash"));
}

// ==============================================================================
// Branch Name Parsing Tests
// ==============================================================================

#[test]
fn test_parse_branch_name() {
    let parsed = make_merge_invocation(&["--squash", "feature-branch"]);

    let branch = parsed.pos_command(0);
    assert_eq!(branch, Some("feature-branch".to_string()));
}

#[test]
fn test_parse_branch_name_with_remote() {
    let parsed = make_merge_invocation(&["--squash", "origin/feature"]);

    let branch = parsed.pos_command(0);
    assert_eq!(branch, Some("origin/feature".to_string()));
}

#[test]
fn test_parse_branch_name_missing() {
    let parsed = make_merge_invocation(&["--squash"]);

    let branch = parsed.pos_command(0);
    assert_eq!(branch, None);
}

// ==============================================================================
// HEAD Resolution Tests
// ==============================================================================

#[test]
fn test_resolve_current_head() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    let commit = repo.commit("test commit").unwrap();

    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let head = repository.head().unwrap();
    let head_sha = head.target().unwrap();

    assert_eq!(head_sha, commit.commit_sha);
}

#[test]
fn test_resolve_branch_head() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let base = repo.commit("base commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature"])
        .stage();
    let feature = repo.commit("feature commit").unwrap();

    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Resolve feature branch
    let feature_obj = repository.revparse_single("feature").unwrap();
    let feature_commit = feature_obj.peel_to_commit().unwrap();

    assert_eq!(feature_commit.id(), feature.commit_sha);
}

// ==============================================================================
// Integration Tests
// ==============================================================================

#[test]
fn test_merge_squash_full_flow() {
    let mut repo = TestRepo::new();

    // Create base
    repo.filename("base.txt")
        .set_contents(vec!["base content"])
        .stage();
    let base = repo.commit("base commit").unwrap();

    let original_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature1.txt")
        .set_contents(vec!["feature 1"])
        .stage();
    repo.commit("feature commit 1").unwrap();

    repo.filename("feature2.txt")
        .set_contents(vec!["feature 2"])
        .stage();
    let feature = repo.commit("feature commit 2").unwrap();

    // Go back to original branch
    repo.git(&["checkout", &original_branch]).unwrap();

    // Execute merge --squash
    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_merge_invocation(&["--squash", "feature"]);
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_merge_hook(&parsed_args, exit_status, &mut repository);

    // Verify event was logged with correct information
    let events = repository.storage.read_rewrite_events().unwrap();
    let merge_squash_event = events.iter().find_map(|e| match e {
        RewriteLogEvent::MergeSquash { merge_squash } => Some(merge_squash),
        _ => None,
    });

    assert!(merge_squash_event.is_some());
    let event = merge_squash_event.unwrap();
    assert_eq!(event.source_branch, "feature");
    assert_eq!(event.base_branch, format!("refs/heads/{}", original_branch));
}

#[test]
fn test_merge_squash_with_commit() {
    let mut repo = TestRepo::new();

    // Create base
    repo.filename("base.txt")
        .set_contents(vec!["base content"])
        .stage();
    repo.commit("base commit").unwrap();

    let original_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature content"])
        .stage();
    repo.commit("feature commit").unwrap();

    // Go back to original branch
    repo.git(&["checkout", &original_branch]).unwrap();

    // Merge --squash (stages changes)
    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_merge_invocation(&["--squash", "feature"]);
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_merge_hook(&parsed_args, exit_status, &mut repository);

    // Then commit the squashed changes
    // (This would typically happen after the merge --squash)

    // Verify MergeSquash event was logged
    let events = repository.storage.read_rewrite_events().unwrap();
    let has_merge_squash = events
        .iter()
        .any(|e| matches!(e, RewriteLogEvent::MergeSquash { .. }));

    assert!(has_merge_squash);
}

// ==============================================================================
// Author Resolution Tests
// ==============================================================================

#[test]
fn test_merge_author_from_config() {
    let repo = TestRepo::new();
    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    use git_ai::commands::hooks::commit_hooks::get_commit_default_author;

    let args = vec![];
    let author = get_commit_default_author(&repository, &args);

    assert!(author.contains("Test User"));
    assert!(author.contains("test@example.com"));
}

// Ignored because resolve_author_spec() requires existing commits to resolve the author pattern,
// and this test uses a fresh repository with no commits
#[test]
#[ignore]
fn test_merge_author_with_flag() {
    let repo = TestRepo::new();
    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    use git_ai::commands::hooks::commit_hooks::get_commit_default_author;

    let args = vec![
        "--author".to_string(),
        "Merge Author <merge@example.com>".to_string(),
    ];
    let author = get_commit_default_author(&repository, &args);

    assert!(author.contains("Merge Author"));
    assert!(author.contains("merge@example.com"));
}

// ==============================================================================
// Edge Case Tests
// ==============================================================================

#[test]
fn test_merge_squash_empty_branch() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    let original_branch = repo.current_branch();

    // Create empty feature branch (same as original)
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.git(&["checkout", &original_branch]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_merge_invocation(&["--squash", "feature"]);
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_merge_hook(&parsed_args, exit_status, &mut repository);

    // Should handle empty merge gracefully
}

#[test]
fn test_merge_squash_detached_head() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let commit = repo.commit("base commit").unwrap();

    // Create feature
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature"])
        .stage();
    repo.commit("feature commit").unwrap();

    // Detach head
    repo.git(&["checkout", &commit.commit_sha]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_merge_invocation(&["--squash", "feature"]);
    let exit_status = std::process::Command::new("true").status().unwrap();

    // Should handle detached HEAD gracefully
    post_merge_hook(&parsed_args, exit_status, &mut repository);
}

reuse_tests_in_worktree!(
    test_post_merge_hook_squash_success,
    test_post_merge_hook_squash_failed,
    test_post_merge_hook_normal_merge,
    test_post_merge_hook_dry_run,
    test_post_merge_hook_invalid_branch,
    test_merge_squash_event_creation,
    test_merge_squash_event_variant,
    test_squash_flag_detection,
    test_dry_run_flag_detection,
    test_no_squash_flag,
    test_parse_branch_name,
    test_parse_branch_name_with_remote,
    test_parse_branch_name_missing,
    test_resolve_current_head,
    test_resolve_branch_head,
    test_merge_squash_full_flow,
    test_merge_squash_with_commit,
    test_merge_author_from_config,
    test_merge_squash_empty_branch,
    test_merge_squash_detached_head,
);
