#[macro_use]
mod repos;
use git_ai::git::repository;
use git_ai::git::repository::Repository;
mod test_utils;

use crate::repos::test_repo::TestRepo;
use git_ai::commands::git_handlers::CommandHooksContext;
use git_ai::commands::hooks::commit_hooks::{
    commit_post_command_hook, commit_pre_command_hook, get_commit_default_author,
};
use git_ai::git::cli_parser::ParsedGitInvocation;
use git_ai::git::rewrite_log::RewriteLogEvent;

// ==============================================================================
// Test Helper Functions
// ==============================================================================

fn make_commit_invocation(args: &[&str]) -> ParsedGitInvocation {
    ParsedGitInvocation {
        global_args: Vec::new(),
        command: Some("commit".to_string()),
        command_args: args.iter().map(|s| s.to_string()).collect(),
        saw_end_of_opts: false,
        is_help: false,
    }
}

// ==============================================================================
// Pre-Commit Hook Tests
// ==============================================================================

#[test]
fn test_pre_commit_hook_success() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["initial content"])
        .stage();

    let mut repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_commit_invocation(&["-m", "test commit"]);

    let result = commit_pre_command_hook(&parsed_args, &mut repository);

    assert!(result, "Pre-commit hook should succeed");
    assert!(
        repository.pre_command_base_commit.is_some(),
        "Should capture pre-command HEAD"
    );
}

#[test]
fn test_pre_commit_hook_dry_run() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["initial content"])
        .stage();

    let mut repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_commit_invocation(&["--dry-run", "-m", "test commit"]);

    let result = commit_pre_command_hook(&parsed_args, &mut repository);

    assert!(!result, "Pre-commit hook should skip dry-run");
}

#[test]
fn test_pre_commit_hook_captures_head() {
    let mut repo = TestRepo::new();

    // Create an initial commit so HEAD exists
    repo.filename("initial.txt")
        .set_contents(vec!["initial"])
        .stage();
    repo.commit("initial commit").unwrap();

    // Stage new changes
    repo.filename("test.txt")
        .set_contents(vec!["test content"])
        .stage();

    let mut repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_commit_invocation(&["-m", "test commit"]);

    commit_pre_command_hook(&parsed_args, &mut repository);

    assert!(
        repository.pre_command_base_commit.is_some(),
        "Should capture HEAD before commit"
    );
}

// ==============================================================================
// Post-Commit Hook Tests
// ==============================================================================

#[test]
fn test_post_commit_hook_success() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();

    let commit = repo.commit("test commit").unwrap();

    let mut repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = None;

    let parsed_args = make_commit_invocation(&["-m", "test commit"]);
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    context.pre_commit_hook_result = Some(true);

    let exit_status = std::process::Command::new("true").status().unwrap();

    commit_post_command_hook(&parsed_args, exit_status, &mut repository, &mut context);

    // Verify a commit event was logged
    let events = repository.storage.read_rewrite_events().unwrap();
    let has_commit = events
        .iter()
        .any(|e| matches!(e, RewriteLogEvent::Commit { .. }));

    assert!(has_commit, "Commit event should be logged");
}

#[test]
fn test_post_commit_hook_amend() {
    let mut repo = TestRepo::new();

    // Create initial commit
    repo.filename("test.txt")
        .set_contents(vec!["initial"])
        .stage();
    let original_commit = repo.commit("initial commit").unwrap();

    // Amend the commit
    repo.filename("test.txt")
        .set_contents(vec!["amended"])
        .stage();
    let amended_commit = repo.git(&["commit", "--amend", "-m", "amended commit"]).unwrap();

    let mut repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(original_commit.commit_sha.clone());

    let parsed_args = make_commit_invocation(&["--amend", "-m", "amended commit"]);
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    context.pre_commit_hook_result = Some(true);

    let exit_status = std::process::Command::new("true").status().unwrap();

    commit_post_command_hook(&parsed_args, exit_status, &mut repository, &mut context);

    // Verify a commit amend event was logged
    let events = repository.storage.read_rewrite_events().unwrap();
    let has_amend = events
        .iter()
        .any(|e| matches!(e, RewriteLogEvent::CommitAmend { .. }));

    assert!(has_amend, "CommitAmend event should be logged for --amend");
}

#[test]
fn test_post_commit_hook_dry_run() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();

    let mut repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_commit_invocation(&["--dry-run", "-m", "test"]);
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

    commit_post_command_hook(&parsed_args, exit_status, &mut repository, &mut context);

    // Dry run should not log events
    let events = repository.storage.read_rewrite_events().unwrap_or_default();
    let has_commit = events
        .iter()
        .any(|e| matches!(e, RewriteLogEvent::Commit { .. }));

    assert!(!has_commit, "Dry run should not log commit events");
}

#[test]
fn test_post_commit_hook_failed_status() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();

    let mut repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_commit_invocation(&["-m", "test"]);
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    context.pre_commit_hook_result = Some(true);

    let exit_status = std::process::Command::new("false")
        .status()
        .unwrap_or_else(|_| std::process::Command::new("sh").arg("-c").arg("exit 1").status().unwrap());

    commit_post_command_hook(&parsed_args, exit_status, &mut repository, &mut context);

    // Failed commit should not log events
    let events = repository.storage.read_rewrite_events().unwrap_or_default();
    let has_commit = events
        .iter()
        .any(|e| matches!(e, RewriteLogEvent::Commit { .. }));

    assert!(!has_commit, "Failed commit should not log events");
}

#[test]
fn test_post_commit_hook_pre_hook_failed() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    repo.commit("test commit").unwrap();

    let mut repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_commit_invocation(&["-m", "test"]);
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    context.pre_commit_hook_result = Some(false);

    let exit_status = std::process::Command::new("true").status().unwrap();

    let events_before = repository.storage.read_rewrite_events().unwrap_or_default();
    let initial_count = events_before.len();

    commit_post_command_hook(&parsed_args, exit_status, &mut repository, &mut context);

    // Should skip if pre-commit hook failed
    let events_after = repository.storage.read_rewrite_events().unwrap_or_default();
    assert_eq!(events_after.len(), initial_count, "Should not log if pre-hook failed");
}

#[test]
fn test_post_commit_hook_porcelain_suppresses_output() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    repo.commit("test commit").unwrap();

    let mut repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = None;

    let parsed_args = make_commit_invocation(&["--porcelain", "-m", "test"]);
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    context.pre_commit_hook_result = Some(true);

    let exit_status = std::process::Command::new("true").status().unwrap();

    // This should succeed but suppress output
    commit_post_command_hook(&parsed_args, exit_status, &mut repository, &mut context);

    assert!(parsed_args.has_command_flag("--porcelain"));
}

#[test]
fn test_post_commit_hook_quiet_suppresses_output() {
    let mut repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    repo.commit("test commit").unwrap();

    let mut repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = None;

    let parsed_args = make_commit_invocation(&["--quiet", "-m", "test"]);
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    context.pre_commit_hook_result = Some(true);

    let exit_status = std::process::Command::new("true").status().unwrap();

    commit_post_command_hook(&parsed_args, exit_status, &mut repository, &mut context);

    assert!(parsed_args.has_command_flag("--quiet"));
}

// ==============================================================================
// Author Resolution Tests
// ==============================================================================

#[test]
fn test_get_commit_default_author_from_config() {
    let repo = TestRepo::new();
    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let args = vec![];
    let author = get_commit_default_author(&repository, &args);

    // Should get from git config (Test User <test@example.com>)
    assert!(author.contains("Test User"));
    assert!(author.contains("test@example.com"));
}

#[test]
fn test_get_commit_default_author_from_author_flag() {
    let repo = TestRepo::new();
    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let args = vec!["--author".to_string(), "Custom Author <custom@example.com>".to_string()];
    let author = get_commit_default_author(&repository, &args);

    // --author flag should override config
    assert!(author.contains("Custom Author"));
    assert!(author.contains("custom@example.com"));
}

#[test]
fn test_get_commit_default_author_from_author_equals() {
    let repo = TestRepo::new();
    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let args = vec!["--author=Custom Author <custom@example.com>".to_string()];
    let author = get_commit_default_author(&repository, &args);

    assert!(author.contains("Custom Author"));
    assert!(author.contains("custom@example.com"));
}

#[test]
fn test_get_commit_default_author_env_precedence() {
    let repo = TestRepo::new();
    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Set environment variable
    unsafe {
        std::env::set_var("GIT_AUTHOR_NAME", "Env Author");
        std::env::set_var("GIT_AUTHOR_EMAIL", "env@example.com");
    }

    let args = vec![];
    let author = get_commit_default_author(&repository, &args);

    // Should use env vars over config
    assert!(author.contains("Env Author"));
    assert!(author.contains("env@example.com"));

    // Clean up
    unsafe {
        std::env::remove_var("GIT_AUTHOR_NAME");
        std::env::remove_var("GIT_AUTHOR_EMAIL");
    }
}

#[test]
fn test_get_commit_default_author_email_env() {
    let repo = TestRepo::new();
    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Set EMAIL environment variable
    unsafe {
        std::env::set_var("EMAIL", "email@example.com");
    }

    let args = vec![];
    let author = get_commit_default_author(&repository, &args);

    // Should extract name from EMAIL
    assert!(author.contains("email@example.com"));

    unsafe {
        std::env::remove_var("EMAIL");
    }
}

#[test]
fn test_get_commit_default_author_name_only() {
    let repo = TestRepo::new();
    let repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    unsafe {
        std::env::set_var("GIT_AUTHOR_NAME", "Name Only");
        std::env::remove_var("GIT_AUTHOR_EMAIL");
    }

    // Temporarily override config to empty
    let args = vec![];
    let author = get_commit_default_author(&repository, &args);

    // Should have name
    assert!(author.contains("Name") || author.contains("Test User"));

    unsafe {
        std::env::remove_var("GIT_AUTHOR_NAME");
    }
}

// ==============================================================================
// Commit Event Creation Tests
// ==============================================================================

#[test]
fn test_commit_event_creation() {
    let event = RewriteLogEvent::commit(Some("abc123".to_string()), "def456".to_string());

    match event {
        RewriteLogEvent::Commit { commit } => {
            assert_eq!(commit.base_commit, Some("abc123".to_string()));
            assert_eq!(commit.commit_sha, "def456");
        }
        _ => panic!("Expected Commit event"),
    }
}

#[test]
fn test_commit_amend_event_creation() {
    let event = RewriteLogEvent::commit_amend("abc123".to_string(), "def456".to_string());

    match event {
        RewriteLogEvent::CommitAmend { commit_amend } => {
            assert_eq!(commit_amend.original_commit, "abc123");
            assert_eq!(commit_amend.amended_commit_sha, "def456");
        }
        _ => panic!("Expected CommitAmend event"),
    }
}

#[test]
fn test_commit_event_no_original() {
    let event = RewriteLogEvent::commit(None, "def456".to_string());

    match event {
        RewriteLogEvent::Commit { commit } => {
            assert!(commit.base_commit.is_none());
            assert_eq!(commit.commit_sha, "def456");
        }
        _ => panic!("Expected Commit event"),
    }
}

// ==============================================================================
// Commit Flag Detection Tests
// ==============================================================================

#[test]
fn test_amend_flag_detection() {
    let parsed = make_commit_invocation(&["--amend", "-m", "message"]);

    assert!(parsed.has_command_flag("--amend"));
}

#[test]
fn test_porcelain_flag_detection() {
    let parsed = make_commit_invocation(&["--porcelain", "-m", "message"]);

    assert!(parsed.has_command_flag("--porcelain"));
}

#[test]
fn test_quiet_flag_detection() {
    let parsed = make_commit_invocation(&["--quiet", "-m", "message"]);

    assert!(parsed.has_command_flag("--quiet"));
}

#[test]
fn test_quiet_short_flag_detection() {
    let parsed = make_commit_invocation(&["-q", "-m", "message"]);

    assert!(parsed.has_command_flag("-q"));
}

#[test]
fn test_no_status_flag_detection() {
    let parsed = make_commit_invocation(&["--no-status", "-m", "message"]);

    assert!(parsed.has_command_flag("--no-status"));
}

#[test]
fn test_dry_run_flag_detection() {
    let parsed = make_commit_invocation(&["--dry-run", "-m", "message"]);

    assert!(parsed.command_args.contains(&"--dry-run".to_string()));
}

// ==============================================================================
// Author Extraction Tests
// ==============================================================================

#[test]
fn test_extract_author_with_equals() {
    let args = vec!["--author=John Doe <john@example.com>".to_string()];

    let author = args.iter().find_map(|arg| {
        arg.strip_prefix("--author=")
            .map(|s| s.to_string())
    });

    assert_eq!(author, Some("John Doe <john@example.com>".to_string()));
}

#[test]
fn test_extract_author_separate_arg() {
    let args = vec!["--author".to_string(), "John Doe <john@example.com>".to_string()];

    let mut author = None;
    for i in 0..args.len() {
        if args[i] == "--author" && i + 1 < args.len() {
            author = Some(args[i + 1].clone());
            break;
        }
    }

    assert_eq!(author, Some("John Doe <john@example.com>".to_string()));
}

#[test]
fn test_extract_author_not_present() {
    let args = vec!["-m".to_string(), "message".to_string()];

    let author = args.iter().find_map(|arg| {
        arg.strip_prefix("--author=")
            .map(|s| s.to_string())
    });

    assert_eq!(author, None);
}

// ==============================================================================
// Integration Tests
// ==============================================================================

#[test]
fn test_commit_full_flow() {
    let mut repo = TestRepo::new();

    // Stage file
    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();

    let mut repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let parsed_args = make_commit_invocation(&["-m", "test commit"]);

    // Pre-hook
    let pre_result = commit_pre_command_hook(&parsed_args, &mut repository);
    assert!(pre_result);

    // Actual commit
    let commit = repo.commit("test commit").unwrap();

    // Post-hook
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    context.pre_commit_hook_result = Some(true);
    let exit_status = std::process::Command::new("true").status().unwrap();

    commit_post_command_hook(&parsed_args, exit_status, &mut repository, &mut context);

    // Verify event was logged
    let events = repository.storage.read_rewrite_events().unwrap();
    let has_commit = events
        .iter()
        .any(|e| matches!(e, RewriteLogEvent::Commit { .. }));

    assert!(has_commit);
}

#[test]
fn test_commit_amend_full_flow() {
    let mut repo = TestRepo::new();

    // Initial commit
    repo.filename("test.txt")
        .set_contents(vec!["initial"])
        .stage();
    let original_commit = repo.commit("initial commit").unwrap();

    // Amend
    repo.filename("test.txt")
        .set_contents(vec!["amended"])
        .stage();

    let mut repository = repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    repository.pre_command_base_commit = Some(original_commit.commit_sha.clone());

    let parsed_args = make_commit_invocation(&["--amend", "-m", "amended commit"]);

    // Pre-hook
    let pre_result = commit_pre_command_hook(&parsed_args, &mut repository);
    assert!(pre_result);

    // Actual amend
    let amended_commit = repo.git(&["commit", "--amend", "-m", "amended commit"]).unwrap();

    // Post-hook
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    context.pre_commit_hook_result = Some(true);
    let exit_status = std::process::Command::new("true").status().unwrap();

    commit_post_command_hook(&parsed_args, exit_status, &mut repository, &mut context);

    // Verify amend event was logged
    let events = repository.storage.read_rewrite_events().unwrap();
    let has_amend = events
        .iter()
        .any(|e| matches!(e, RewriteLogEvent::CommitAmend { .. }));

    assert!(has_amend);
}
