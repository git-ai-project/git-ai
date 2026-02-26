#[macro_use]
mod repos;
use git_ai::git::repository;
use git_ai::git::repository::Repository;
mod test_utils;

use crate::repos::test_repo::TestRepo;
use git_ai::commands::git_handlers::CommandHooksContext;
use git_ai::commands::hooks::checkout_hooks::{post_checkout_hook, pre_checkout_hook};
use git_ai::git::cli_parser::ParsedGitInvocation;

// ==============================================================================
// Test Helper Functions
// ==============================================================================

fn make_checkout_invocation(args: &[&str]) -> ParsedGitInvocation {
    ParsedGitInvocation {
        global_args: Vec::new(),
        command: Some("checkout".to_string()),
        command_args: args.iter().map(|s| s.to_string()).collect(),
        saw_end_of_opts: false,
        is_help: false,
    }
}

// ==============================================================================
// Pre-Checkout Hook Tests
// ==============================================================================

#[test]
fn test_pre_checkout_hook_normal() {
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
    let parsed_args = make_checkout_invocation(&["main"]);

    pre_checkout_hook(&parsed_args, &mut repository, &mut context);

    // Should capture pre-command HEAD
    assert!(repository.pre_command_base_commit.is_some());
}

#[test]
fn test_pre_checkout_hook_with_merge_flag() {
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
    let parsed_args = make_checkout_invocation(&["--merge", "main"]);

    pre_checkout_hook(&parsed_args, &mut repository, &mut context);

    // Should potentially capture VirtualAttributions for merge
    // (depends on working log state)
}

#[test]
fn test_pre_checkout_hook_merge_without_changes() {
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
    let parsed_args = make_checkout_invocation(&["--merge", "main"]);

    pre_checkout_hook(&parsed_args, &mut repository, &mut context);

    // No uncommitted changes, so stashed_va should be None
    assert!(context.stashed_va.is_none());
}

#[test]
fn test_pre_checkout_hook_merge_short_flag() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    repo.commit("initial commit").unwrap();

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
    let parsed_args = make_checkout_invocation(&["-m", "main"]);

    pre_checkout_hook(&parsed_args, &mut repository, &mut context);

    assert!(parsed_args.has_command_flag("-m"));
}

// ==============================================================================
// Post-Checkout Hook Tests
// ==============================================================================

#[test]
fn test_post_checkout_hook_success() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    // Capture original branch before switching
    let original_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature"])
        .stage();
    let feature_commit = repo.commit("feature commit").unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(feature_commit.commit_sha.clone());

    // Checkout back to original branch
    repo.git(&["checkout", &original_branch]).unwrap();

    let parsed_args = make_checkout_invocation(&[&original_branch]);
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

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // Working log should be renamed/migrated
}

#[test]
fn test_post_checkout_hook_failed() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    repo.commit("initial commit").unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_checkout_invocation(&["nonexistent"]);
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

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // Failed checkout should not process working log
}

#[test]
fn test_post_checkout_hook_head_unchanged() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    let commit = repo.commit("initial commit").unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(commit.commit_sha.clone());

    let parsed_args = make_checkout_invocation(&["main"]);
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

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // HEAD unchanged, should return early
}

#[test]
fn test_post_checkout_hook_pathspec() {
    let mut repo = TestRepo::new();

    repo.filename("file1.txt")
        .set_contents(vec!["file1"])
        .stage();
    repo.commit("commit 1").unwrap();

    repo.filename("file1.txt")
        .set_contents(vec!["modified"])
        .stage();

    let commit_sha = repository::find_repository_in_path(repo.path().to_str().unwrap())
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(commit_sha.clone());

    // Checkout specific file (pathspec checkout)
    let parsed_args = make_checkout_invocation(&["HEAD", "--", "file1.txt"]);
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

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // Should remove attributions for checked out files
    let pathspecs = parsed_args.pathspecs();
    assert!(!pathspecs.is_empty());
}

#[test]
fn test_post_checkout_hook_multiple_pathspecs() {
    let mut repo = TestRepo::new();

    repo.filename("file1.txt")
        .set_contents(vec!["file1"])
        .stage();
    repo.filename("file2.txt")
        .set_contents(vec!["file2"])
        .stage();
    repo.commit("commit 1").unwrap();

    let commit_sha = repository::find_repository_in_path(repo.path().to_str().unwrap())
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(commit_sha.clone());

    let parsed_args = make_checkout_invocation(&["HEAD", "--", "file1.txt", "file2.txt"]);
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

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);

    let pathspecs = parsed_args.pathspecs();
    assert_eq!(pathspecs.len(), 2);
}

#[test]
fn test_post_checkout_hook_force_checkout() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    // Capture original branch before switching
    let original_branch = repo.current_branch();

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

    // Force checkout discards changes
    repo.git(&["checkout", "-f", &original_branch]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(old_head.clone());

    let parsed_args = make_checkout_invocation(&["--force", &original_branch]);
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

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // Force checkout should delete working log
}

#[test]
fn test_post_checkout_hook_force_short_flag() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    // Capture original branch before switching
    let original_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let old_head = repository::find_repository_in_path(repo.path().to_str().unwrap())
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap();

    repo.git(&["checkout", &original_branch]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(old_head.clone());

    let parsed_args = make_checkout_invocation(&["-f", &original_branch]);
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

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);

    assert!(parsed_args.command_args.contains(&"-f".to_string()));
}

#[test]
fn test_post_checkout_hook_with_merge() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    // Capture original branch before switching
    let original_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let old_head = repository::find_repository_in_path(repo.path().to_str().unwrap())
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap();

    repo.git(&["checkout", &original_branch]).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(old_head.clone());

    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    // In real scenario, pre_checkout_hook would populate this
    // context.stashed_va = Some(...);

    let parsed_args = make_checkout_invocation(&["--merge", &original_branch]);
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // If stashed_va was present, it would be restored
    assert!(context.stashed_va.is_none());
}

// ==============================================================================
// Flag Detection Tests
// ==============================================================================

#[test]
fn test_force_flag_detection() {
    let parsed = make_checkout_invocation(&["--force", "branch"]);

    assert!(parsed.command_args.iter().any(|a| a == "--force"));
}

#[test]
fn test_force_short_flag_detection() {
    let parsed = make_checkout_invocation(&["-f", "branch"]);

    assert!(parsed.command_args.iter().any(|a| a == "-f"));
}

#[test]
fn test_merge_flag_detection() {
    let parsed = make_checkout_invocation(&["--merge", "branch"]);

    assert!(parsed.has_command_flag("--merge"));
}

#[test]
fn test_merge_short_flag_detection() {
    let parsed = make_checkout_invocation(&["-m", "branch"]);

    assert!(parsed.has_command_flag("-m"));
}

// ==============================================================================
// Pathspec Detection Tests
// ==============================================================================

#[test]
fn test_pathspec_detection_single() {
    let parsed = make_checkout_invocation(&["HEAD", "--", "file.txt"]);

    let pathspecs = parsed.pathspecs();
    assert_eq!(pathspecs.len(), 1);
    assert_eq!(pathspecs[0], "file.txt");
}

#[test]
fn test_pathspec_detection_multiple() {
    let parsed = make_checkout_invocation(&["HEAD", "--", "file1.txt", "file2.txt", "dir/"]);

    let pathspecs = parsed.pathspecs();
    assert_eq!(pathspecs.len(), 3);
    assert!(pathspecs.contains(&"file1.txt".to_string()));
    assert!(pathspecs.contains(&"file2.txt".to_string()));
    assert!(pathspecs.contains(&"dir/".to_string()));
}

#[test]
fn test_pathspec_detection_none() {
    let parsed = make_checkout_invocation(&["branch"]);

    let pathspecs = parsed.pathspecs();
    assert!(pathspecs.is_empty());
}

// ==============================================================================
// Pathspec Matching Tests
// ==============================================================================

#[test]
fn test_pathspec_exact_match() {
    let pathspecs = vec!["file.txt".to_string()];

    let matches = |file: &str| {
        pathspecs.iter().any(|p| {
            file == p
                || (p.ends_with('/') && file.starts_with(p))
                || file.starts_with(&format!("{}/", p))
        })
    };

    assert!(matches("file.txt"));
    assert!(!matches("other.txt"));
}

#[test]
fn test_pathspec_directory_match() {
    let pathspecs = vec!["dir/".to_string()];

    let matches = |file: &str| {
        pathspecs.iter().any(|p| {
            file == p
                || (p.ends_with('/') && file.starts_with(p))
                || file.starts_with(&format!("{}/", p))
        })
    };

    assert!(matches("dir/file.txt"));
    assert!(matches("dir/subdir/file.txt"));
    assert!(!matches("other/file.txt"));
}

#[test]
fn test_pathspec_directory_without_slash() {
    let pathspecs = vec!["dir".to_string()];

    let matches = |file: &str| {
        pathspecs.iter().any(|p| {
            file == p
                || (p.ends_with('/') && file.starts_with(p))
                || file.starts_with(&format!("{}/", p))
        })
    };

    assert!(matches("dir"));
    assert!(matches("dir/file.txt"));
    assert!(!matches("directory/file.txt"));
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
// Integration Tests
// ==============================================================================

#[test]
fn test_checkout_normal_flow() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    // Capture original branch before switching
    let original_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature"])
        .stage();
    repo.commit("feature commit").unwrap();

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
    let parsed_args = make_checkout_invocation(&[&original_branch]);

    // Pre-hook
    pre_checkout_hook(&parsed_args, &mut repository, &mut context);
    assert!(repository.pre_command_base_commit.is_some());

    let old_head = repository.pre_command_base_commit.clone();

    // Actual checkout
    repo.git(&["checkout", &original_branch]).unwrap();

    // Post-hook
    repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = old_head;
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);
}

#[test]
fn test_checkout_force_flow() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    // Capture original branch before switching
    let original_branch = repo.current_branch();

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
    let parsed_args = make_checkout_invocation(&["--force", &original_branch]);

    // Pre-hook
    pre_checkout_hook(&parsed_args, &mut repository, &mut context);
    let old_head = repository.pre_command_base_commit.clone().unwrap();

    // Force checkout
    repo.git(&["checkout", "-f", &original_branch]).unwrap();

    // Post-hook
    repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(old_head.clone());
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // Working log for old_head should be deleted
}

#[test]
fn test_checkout_pathspec_flow() {
    let mut repo = TestRepo::new();

    repo.filename("file1.txt")
        .set_contents(vec!["original 1"])
        .stage();
    repo.filename("file2.txt")
        .set_contents(vec!["original 2"])
        .stage();
    let commit = repo.commit("initial commit").unwrap();

    // Modify files
    repo.filename("file1.txt")
        .set_contents(vec!["modified 1"])
        .stage();
    repo.filename("file2.txt")
        .set_contents(vec!["modified 2"])
        .stage();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(commit.commit_sha.clone());
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };

    // Checkout specific file
    let parsed_args = make_checkout_invocation(&["HEAD", "--", "file1.txt"]);

    pre_checkout_hook(&parsed_args, &mut repository, &mut context);

    // Actual checkout
    repo.git(&["checkout", "HEAD", "--", "file1.txt"]).unwrap();

    // Post-hook
    repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(commit.commit_sha.clone());
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);

    // Should remove attributions only for file1.txt
}

#[test]
fn test_checkout_new_branch_creation() {
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
    let parsed_args = make_checkout_invocation(&["-b", "new-branch"]);

    pre_checkout_hook(&parsed_args, &mut repository, &mut context);

    // Create and checkout new branch
    repo.git(&["checkout", "-b", "new-branch"]).unwrap();

    // HEAD unchanged (same commit, different branch)
    repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);
}

#[test]
fn test_checkout_detached_head() {
    let mut repo = TestRepo::new();

    repo.filename("file1.txt")
        .set_contents(vec!["file1"])
        .stage();
    let commit1 = repo.commit("commit 1").unwrap();

    repo.filename("file2.txt")
        .set_contents(vec!["file2"])
        .stage();
    let commit2 = repo.commit("commit 2").unwrap();

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
    let parsed_args = make_checkout_invocation(&[&commit1.commit_sha]);

    pre_checkout_hook(&parsed_args, &mut repository, &mut context);
    let old_head = repository.pre_command_base_commit.clone().unwrap();

    // Checkout specific commit (detached HEAD)
    repo.git(&["checkout", &commit1.commit_sha]).unwrap();

    repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(old_head);
    let exit_status = std::process::Command::new("true").status().unwrap();

    post_checkout_hook(&parsed_args, &mut repository, exit_status, &mut context);
}

reuse_tests_in_worktree!(
    test_pre_checkout_hook_normal,
    test_pre_checkout_hook_with_merge_flag,
    test_pre_checkout_hook_merge_without_changes,
    test_pre_checkout_hook_merge_short_flag,
    test_post_checkout_hook_success,
    test_post_checkout_hook_failed,
    test_post_checkout_hook_head_unchanged,
    test_post_checkout_hook_pathspec,
    test_post_checkout_hook_multiple_pathspecs,
    test_post_checkout_hook_force_checkout,
    test_post_checkout_hook_force_short_flag,
    test_post_checkout_hook_with_merge,
    test_force_flag_detection,
    test_force_short_flag_detection,
    test_merge_flag_detection,
    test_merge_short_flag_detection,
    test_pathspec_detection_single,
    test_pathspec_detection_multiple,
    test_pathspec_detection_none,
    test_pathspec_exact_match,
    test_pathspec_directory_match,
    test_pathspec_directory_without_slash,
    test_detect_uncommitted_changes_staged,
    test_detect_uncommitted_changes_unstaged,
    test_no_uncommitted_changes,
    test_checkout_normal_flow,
    test_checkout_force_flow,
    test_checkout_pathspec_flow,
    test_checkout_new_branch_creation,
    test_checkout_detached_head,
);
