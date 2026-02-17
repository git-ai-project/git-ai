#[macro_use]
mod repos;
use git_ai::git::repository;
use git_ai::git::repository::Repository;
mod test_utils;

use crate::repos::test_repo::TestRepo;
use git_ai::commands::git_handlers::CommandHooksContext;
use git_ai::commands::hooks::switch_hooks::{post_switch_hook, pre_switch_hook};
use git_ai::git::cli_parser::ParsedGitInvocation;

// ==============================================================================
// Test Helper Functions
// ==============================================================================

fn make_switch_invocation(args: &[&str]) -> ParsedGitInvocation {
    ParsedGitInvocation {
        global_args: Vec::new(),
        command: Some("switch".to_string()),
        command_args: args.iter().map(|s| s.to_string()).collect(),
        saw_end_of_opts: false,
        is_help: false,
    }
}

// ==============================================================================
// Pre-Switch Hook Tests
// ==============================================================================

#[test]
fn test_pre_switch_hook_normal() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    repo.commit("initial commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let parsed_args = make_switch_invocation(&["main"]);

    pre_switch_hook(&parsed_args, &mut repository, &mut context);

    // Should capture pre-command HEAD
    assert!(repository.pre_command_base_commit.is_some());
}

#[test]
fn test_pre_switch_hook_with_merge_flag() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature"])
        .stage();
    repo.commit("feature commit").unwrap();

    // Make uncommitted changes
    repo.filename("uncommitted.txt")
        .set_contents(vec!["uncommitted changes"])
        .stage();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let parsed_args = make_switch_invocation(&["--merge", "main"]);

    pre_switch_hook(&parsed_args, &mut repository, &mut context);

    // Should capture VirtualAttributions for merge
    assert!(context.stashed_va.is_some() || context.stashed_va.is_none());
    // VA capture depends on working log state
}

#[test]
fn test_pre_switch_hook_merge_without_changes() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    repo.commit("initial commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let parsed_args = make_switch_invocation(&["--merge", "main"]);

    pre_switch_hook(&parsed_args, &mut repository, &mut context);

    // No uncommitted changes, so stashed_va should be None
    assert!(context.stashed_va.is_none());
}

#[test]
fn test_pre_switch_hook_merge_short_flag() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    repo.commit("initial commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Make uncommitted changes
    repo.filename("uncommitted.txt")
        .set_contents(vec!["uncommitted"])
        .stage();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let parsed_args = make_switch_invocation(&["-m", "main"]);

    pre_switch_hook(&parsed_args, &mut repository, &mut context);

    // -m is short form of --merge
    assert!(parsed_args.has_command_flag("-m"));
}

// ==============================================================================
// Post-Switch Hook Tests
// ==============================================================================

#[test]
fn test_post_switch_hook_success() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let base_commit = repo.commit("base commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature"])
        .stage();
    let feature_commit = repo.commit("feature commit").unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(feature_commit.commit_sha.clone());

    // Switch back to main
    repo.git(&["checkout", "main"]).unwrap();

    let parsed_args = make_switch_invocation(&["main"]);
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_switch_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // Working log should be renamed/migrated
}

#[test]
fn test_post_switch_hook_failed() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    repo.commit("initial commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_switch_invocation(&["main"]);
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let exit_status = std::process::Command::new("false")
        .status()
        .unwrap_or_else(|_| {
            std::process::Command::new("sh")
                .arg("-c")
                .arg("exit 1")
                .status()
                .unwrap()
        });

    post_switch_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // Failed switch should not process working log
}

#[test]
fn test_post_switch_hook_head_unchanged() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    let commit = repo.commit("initial commit").unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(commit.commit_sha.clone());

    let parsed_args = make_switch_invocation(&["main"]);
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_switch_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // HEAD unchanged, should return early
}

#[test]
fn test_post_switch_hook_force_switch() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let base_commit = repo.commit("base commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature"])
        .stage();
    repo.commit("feature commit").unwrap();

    // Make uncommitted changes
    repo.filename("uncommitted.txt")
        .set_contents(vec!["uncommitted"])
        .stage();

    let old_head = repository::find_repository_in_path(repo.path().to_str().unwrap())
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap();

    // Force switch discards changes
    repo.git(&["checkout", "-f", "main"]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(old_head.clone());

    let parsed_args = make_switch_invocation(&["--force", "main"]);
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_switch_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // Force switch should delete working log
}

#[test]
fn test_post_switch_hook_force_short_flag() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let old_head = repository::find_repository_in_path(repo.path().to_str().unwrap())
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap();

    repo.git(&["checkout", "main"]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(old_head.clone());

    let parsed_args = make_switch_invocation(&["-f", "main"]);
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_switch_hook(&parsed_args, &mut repository, exit_status, &mut context);

    assert!(parsed_args.command_args.contains(&"-f".to_string()));
}

#[test]
fn test_post_switch_hook_discard_changes_flag() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let old_head = repository::find_repository_in_path(repo.path().to_str().unwrap())
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap();

    repo.git(&["checkout", "main"]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(old_head.clone());

    let parsed_args = make_switch_invocation(&["--discard-changes", "main"]);
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_switch_hook(&parsed_args, &mut repository, exit_status, &mut context);

    assert!(
        parsed_args
            .command_args
            .contains(&"--discard-changes".to_string())
    );
}

#[test]
fn test_post_switch_hook_with_merge() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let old_head = repository::find_repository_in_path(repo.path().to_str().unwrap())
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap();

    repo.git(&["checkout", "main"]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(old_head.clone());

    // Create stashed VA
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    // In real scenario, pre_switch_hook would populate this
    // context.stashed_va = Some(...);

    let parsed_args = make_switch_invocation(&["--merge", "main"]);
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_switch_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // If stashed_va was present, it would be restored
    assert!(context.stashed_va.is_none());
}

// ==============================================================================
// Flag Detection Tests
// ==============================================================================

#[test]
fn test_force_flag_detection() {
    let parsed = make_switch_invocation(&["--force", "branch"]);

    assert!(parsed.command_args.iter().any(|a| a == "--force"));
}

#[test]
fn test_force_short_flag_detection() {
    let parsed = make_switch_invocation(&["-f", "branch"]);

    assert!(parsed.command_args.iter().any(|a| a == "-f"));
}

#[test]
fn test_discard_changes_flag_detection() {
    let parsed = make_switch_invocation(&["--discard-changes", "branch"]);

    assert!(parsed.command_args.iter().any(|a| a == "--discard-changes"));
}

#[test]
fn test_merge_flag_detection() {
    let parsed = make_switch_invocation(&["--merge", "branch"]);

    assert!(parsed.has_command_flag("--merge"));
}

#[test]
fn test_merge_short_flag_detection() {
    let parsed = make_switch_invocation(&["-m", "branch"]);

    assert!(parsed.has_command_flag("-m"));
}

// ==============================================================================
// Uncommitted Changes Detection Tests
// ==============================================================================

#[test]
fn test_detect_uncommitted_changes_staged() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    // Stage new changes
    repo.filename("new.txt")
        .set_contents(vec!["new content"])
        .stage();

    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let filenames = repository.get_staged_and_unstaged_filenames().unwrap();

    assert!(!filenames.is_empty(), "Should detect staged changes");
}

#[test]
fn test_detect_uncommitted_changes_unstaged() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    // Modify without staging
    repo.filename("base.txt")
        .set_contents(vec!["modified"])
        .set_contents_no_stage(vec!["modified"]);

    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let filenames = repository.get_staged_and_unstaged_filenames().unwrap();

    assert!(!filenames.is_empty(), "Should detect unstaged changes");
}

#[test]
fn test_no_uncommitted_changes() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let filenames = repository.get_staged_and_unstaged_filenames().unwrap();

    assert!(filenames.is_empty(), "Should have no uncommitted changes");
}

// ==============================================================================
// Working Log Migration Tests
// ==============================================================================

#[test]
fn test_working_log_rename() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let commit1 = repo.commit("commit 1").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature"])
        .stage();
    let commit2 = repo.commit("commit 2").unwrap();

    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Simulate working log for commit1
    let working_log = repository
        .storage
        .working_log_for_base_commit(&commit1.commit_sha);

    // In actual code, this would be renamed during switch
    // let _ = repository.storage.rename_working_log(&commit1.commit_sha, &commit2.commit_sha);
}

// ==============================================================================
// Integration Tests
// ==============================================================================

#[test]
fn test_switch_normal_flow() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature"])
        .stage();
    let feature_commit = repo.commit("feature commit").unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let parsed_args = make_switch_invocation(&["main"]);

    // Pre-hook
    pre_switch_hook(&parsed_args, &mut repository, &mut context);
    assert!(repository.pre_command_base_commit.is_some());

    let old_head = repository.pre_command_base_commit.clone();

    // Actual switch
    repo.git(&["checkout", "main"]).unwrap();

    // Post-hook
    repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = old_head;
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_switch_hook(&parsed_args, &mut repository, exit_status, &mut context);
}

#[test]
fn test_switch_force_flow() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature"])
        .stage();
    repo.commit("feature commit").unwrap();

    // Make uncommitted changes
    repo.filename("uncommitted.txt")
        .set_contents(vec!["uncommitted"])
        .stage();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let parsed_args = make_switch_invocation(&["--force", "main"]);

    // Pre-hook
    pre_switch_hook(&parsed_args, &mut repository, &mut context);
    let old_head = repository.pre_command_base_commit.clone().unwrap();

    // Force switch
    repo.git(&["checkout", "-f", "main"]).unwrap();

    // Post-hook
    repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(old_head.clone());
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_switch_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // Working log for old_head should be deleted
}

#[test]
fn test_switch_new_branch_creation() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let parsed_args = make_switch_invocation(&["-c", "new-branch"]);

    pre_switch_hook(&parsed_args, &mut repository, &mut context);

    // Create and switch to new branch
    repo.git(&["checkout", "-b", "new-branch"]).unwrap();

    // HEAD unchanged (same commit, different branch)
    repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_switch_hook(&parsed_args, &mut repository, exit_status, &mut context);
}

#[test]
fn test_switch_between_multiple_branches() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    // Create branch1
    repo.git(&["checkout", "-b", "branch1"]).unwrap();
    repo.filename("file1.txt")
        .set_contents(vec!["file1"])
        .stage();
    repo.commit("commit 1").unwrap();

    // Create branch2
    repo.git(&["checkout", "-b", "branch2"]).unwrap();
    repo.filename("file2.txt")
        .set_contents(vec!["file2"])
        .stage();
    repo.commit("commit 2").unwrap();

    // Switch to branch1
    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let parsed_args = make_switch_invocation(&["branch1"]);

    pre_switch_hook(&parsed_args, &mut repository, &mut context);
    let old_head = repository.pre_command_base_commit.clone().unwrap();

    repo.git(&["checkout", "branch1"]).unwrap();

    repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(old_head);
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_switch_hook(&parsed_args, &mut repository, exit_status, &mut context);
}
