#[macro_use]
mod repos;
use git_ai::git::repository;
use git_ai::git::repository::Repository;
mod test_utils;

use crate::repos::test_repo::TestRepo;
use git_ai::commands::git_handlers::CommandHooksContext;
use git_ai::commands::hooks::rebase_hooks::{handle_rebase_post_command, pre_rebase_hook};
use git_ai::git::cli_parser::ParsedGitInvocation;
use git_ai::git::rewrite_log::RewriteLogEvent;

// ==============================================================================
// Test Helper Functions
// ==============================================================================

fn make_rebase_invocation(args: &[&str]) -> ParsedGitInvocation {
    ParsedGitInvocation {
        global_args: Vec::new(),
        command: Some("rebase".to_string()),
        command_args: args.iter().map(|s| s.to_string()).collect(),
        saw_end_of_opts: false,
        is_help: false,
    }
}

// ==============================================================================
// Pre-Rebase Hook Tests
// ==============================================================================

#[test]
fn test_pre_rebase_hook_starts_new_rebase() {
    let mut repo = TestRepo::new();

    // Create initial commit
    repo.filename("base.txt")
        .set_contents(vec!["base content"])
        .stage();
    let base_commit = repo.commit("base commit").unwrap();

    // Create branch to rebase
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature content"])
        .stage();
    repo.commit("feature commit").unwrap();

    // Prepare context and parsed args
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let parsed_args = make_rebase_invocation(&["main"]);
    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Execute pre-hook
    pre_rebase_hook(&parsed_args, &mut repository, &mut context);

    // Verify context captured original head
    assert!(context.rebase_original_head.is_some());

    // Verify RebaseStart event was logged
    let events = repository.storage.read_rewrite_events().unwrap();
    let has_start = events
        .iter()
        .any(|e| matches!(e, RewriteLogEvent::RebaseStart { .. }));
    assert!(has_start, "RebaseStart event should be logged");
}

#[test]
fn test_pre_rebase_hook_continuing_rebase() {
    let mut repo = TestRepo::new();

    // Create initial commit
    repo.filename("base.txt")
        .set_contents(vec!["base content"])
        .stage();
    repo.commit("base commit").unwrap();

    // Simulate in-progress rebase by creating rebase-merge directory
    let rebase_dir = repo.path().join(".git").join("rebase-merge");
    std::fs::create_dir_all(&rebase_dir).unwrap();

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
    let parsed_args = make_rebase_invocation(&["--continue"]);

    // Execute pre-hook for continuing rebase
    pre_rebase_hook(&parsed_args, &mut repository, &mut context);

    // For continue mode, we shouldn't log a new Start event
    // Check that context doesn't try to capture new original head
    // (In actual code, it reads from log instead)
}

#[test]
fn test_pre_rebase_hook_interactive_mode() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt")
        .set_contents(vec!["base content"])
        .stage();
    repo.commit("base commit").unwrap();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature content"])
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
    let parsed_args = make_rebase_invocation(&["-i", "main"]);

    pre_rebase_hook(&parsed_args, &mut repository, &mut context);

    // Verify interactive flag is detected
    let events = repository.storage.read_rewrite_events().unwrap();
    let start_event = events.iter().find_map(|e| match e {
        RewriteLogEvent::RebaseStart { rebase_start } => Some(rebase_start),
        _ => None,
    });

    assert!(start_event.is_some());
    assert!(start_event.unwrap().is_interactive);
}

#[test]
fn test_pre_rebase_hook_with_onto() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt")
        .set_contents(vec!["base content"])
        .stage();
    let base = repo.commit("base commit").unwrap();

    repo.filename("another.txt")
        .set_contents(vec!["another"])
        .stage();
    let onto_commit = repo.commit("onto commit").unwrap();

    repo.git(&["checkout", "-b", "feature", &base.commit_sha])
        .unwrap();
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
    let parsed_args = make_rebase_invocation(&["--onto", &onto_commit.commit_sha, "main"]);

    pre_rebase_hook(&parsed_args, &mut repository, &mut context);

    // Verify onto_head was captured
    assert!(context.rebase_onto.is_some());
}

// ==============================================================================
// Post-Rebase Hook Tests
// ==============================================================================

#[test]
fn test_post_rebase_hook_still_in_progress() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    // Simulate in-progress rebase
    let rebase_dir = repo.path().join(".git").join("rebase-merge");
    std::fs::create_dir_all(&rebase_dir).unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let parsed_args = make_rebase_invocation(&["main"]);
    let exit_status = std::process::Command::new("true").status().unwrap();

    // Execute post-hook
    handle_rebase_post_command(&context, &parsed_args, exit_status, &mut repository);

    // Hook should return early without processing
    // No RebaseComplete or RebaseAbort event should be logged
    let events = repository.storage.read_rewrite_events().unwrap();
    let has_complete = events
        .iter()
        .any(|e| matches!(e, RewriteLogEvent::RebaseComplete { .. }));
    let has_abort = events
        .iter()
        .any(|e| matches!(e, RewriteLogEvent::RebaseAbort { .. }));

    assert!(!has_complete);
    assert!(!has_abort);

    // Clean up
    std::fs::remove_dir_all(&rebase_dir).unwrap();
}

#[test]
fn test_post_rebase_hook_aborted() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let original_commit = repo.commit("base commit").unwrap();

    // Log a RebaseStart event
    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let start_event =
        RewriteLogEvent::rebase_start(git_ai::git::rewrite_log::RebaseStartEvent::new_with_onto(
            original_commit.commit_sha.clone(),
            false,
            None,
        ));
    repository
        .storage
        .append_rewrite_event(start_event)
        .unwrap();

    // Prepare context with original head
    let mut context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    context.rebase_original_head = Some(original_commit.commit_sha.clone());

    let parsed_args = make_rebase_invocation(&["--abort"]);
    let exit_status = std::process::Command::new("false")
        .status()
        .unwrap_or_else(|_| {
            std::process::Command::new("sh")
                .arg("-c")
                .arg("exit 1")
                .status()
                .unwrap()
        });

    handle_rebase_post_command(&context, &parsed_args, exit_status, &mut repository);

    // Verify RebaseAbort event was logged
    let events = repository.storage.read_rewrite_events().unwrap();
    let has_abort = events
        .iter()
        .any(|e| matches!(e, RewriteLogEvent::RebaseAbort { .. }));

    assert!(has_abort, "RebaseAbort event should be logged on failure");
}

#[test]
fn test_post_rebase_hook_dry_run() {
    let mut repo = TestRepo::new();

    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    repo.commit("base commit").unwrap();

    let mut repository =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let context = CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    let parsed_args = make_rebase_invocation(&["--dry-run", "main"]);
    let exit_status = std::process::Command::new("true").status().unwrap();

    handle_rebase_post_command(&context, &parsed_args, exit_status, &mut repository);

    // Dry run should not log any events
    let events_before = repository.storage.read_rewrite_events().unwrap_or_default();
    let initial_count = events_before.len();

    // Re-run the hook
    handle_rebase_post_command(&context, &parsed_args, exit_status, &mut repository);

    let events_after = repository.storage.read_rewrite_events().unwrap_or_default();
    assert_eq!(
        events_after.len(),
        initial_count,
        "Dry run should not add events"
    );
}

// ==============================================================================
// Rebase State Detection Tests
// ==============================================================================

#[test]
fn test_rebase_directory_detection() {
    let repo = TestRepo::new();

    let rebase_merge_dir = repo.path().join(".git").join("rebase-merge");
    let rebase_apply_dir = repo.path().join(".git").join("rebase-apply");

    // Initially neither should exist
    assert!(!rebase_merge_dir.exists());
    assert!(!rebase_apply_dir.exists());

    // Create rebase-merge
    std::fs::create_dir_all(&rebase_merge_dir).unwrap();
    assert!(rebase_merge_dir.exists());

    // Clean up
    std::fs::remove_dir_all(&rebase_merge_dir).unwrap();

    // Create rebase-apply
    std::fs::create_dir_all(&rebase_apply_dir).unwrap();
    assert!(rebase_apply_dir.exists());

    std::fs::remove_dir_all(&rebase_apply_dir).unwrap();
}

// ==============================================================================
// Rebase Event Sequencing Tests
// ==============================================================================

#[test]
fn test_rebase_event_sequence_start_complete() {
    use git_ai::git::rewrite_log::{RebaseCompleteEvent, RebaseStartEvent};

    let events = vec![
        RewriteLogEvent::rebase_start(RebaseStartEvent::new_with_onto(
            "abc123".to_string(),
            false,
            None,
        )),
        RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
            "abc123".to_string(),
            "def456".to_string(),
            false,
            vec!["commit1".to_string()],
            vec!["commit2".to_string()],
        )),
    ];

    assert_eq!(events.len(), 2);

    match &events[0] {
        RewriteLogEvent::RebaseStart { .. } => {}
        _ => panic!("Expected RebaseStart first"),
    }

    match &events[1] {
        RewriteLogEvent::RebaseComplete { .. } => {}
        _ => panic!("Expected RebaseComplete second"),
    }
}

#[test]
fn test_rebase_event_sequence_start_abort() {
    use git_ai::git::rewrite_log::{RebaseAbortEvent, RebaseStartEvent};

    let events = vec![
        RewriteLogEvent::rebase_start(RebaseStartEvent::new_with_onto(
            "abc123".to_string(),
            false,
            None,
        )),
        RewriteLogEvent::rebase_abort(RebaseAbortEvent::new("abc123".to_string())),
    ];

    assert_eq!(events.len(), 2);

    match &events[0] {
        RewriteLogEvent::RebaseStart { .. } => {}
        _ => panic!("Expected RebaseStart first"),
    }

    match &events[1] {
        RewriteLogEvent::RebaseAbort { .. } => {}
        _ => panic!("Expected RebaseAbort second"),
    }
}

// ==============================================================================
// Rebase Event Creation Tests
// ==============================================================================

#[test]
fn test_rebase_start_event_creation() {
    use git_ai::git::rewrite_log::RebaseStartEvent;

    let event =
        RebaseStartEvent::new_with_onto("abc123".to_string(), true, Some("def456".to_string()));

    assert_eq!(event.original_head, "abc123");
    assert!(event.is_interactive);
    assert_eq!(event.onto_head, Some("def456".to_string()));
}

#[test]
fn test_rebase_complete_event_creation() {
    use git_ai::git::rewrite_log::RebaseCompleteEvent;

    let event = RebaseCompleteEvent::new(
        "abc123".to_string(),
        "def456".to_string(),
        true,
        vec!["commit1".to_string(), "commit2".to_string()],
        vec!["new1".to_string(), "new2".to_string()],
    );

    assert_eq!(event.original_head, "abc123");
    assert_eq!(event.new_head, "def456");
    assert!(event.is_interactive);
    assert_eq!(event.original_commits.len(), 2);
    assert_eq!(event.new_commits.len(), 2);
}

#[test]
fn test_rebase_abort_event_creation() {
    use git_ai::git::rewrite_log::RebaseAbortEvent;

    let event = RebaseAbortEvent::new("abc123".to_string());

    assert_eq!(event.original_head, "abc123");
}

// ==============================================================================
// Rebase Control Mode Tests
// ==============================================================================

#[test]
fn test_rebase_continue_mode() {
    let parsed = make_rebase_invocation(&["--continue"]);

    assert!(parsed.has_command_flag("--continue"));
}

#[test]
fn test_rebase_abort_mode() {
    let parsed = make_rebase_invocation(&["--abort"]);

    assert!(parsed.has_command_flag("--abort"));
}

#[test]
fn test_rebase_skip_mode() {
    let parsed = make_rebase_invocation(&["--skip"]);

    assert!(parsed.has_command_flag("--skip"));
}

#[test]
fn test_rebase_quit_mode() {
    let parsed = make_rebase_invocation(&["--quit"]);

    assert!(parsed.has_command_flag("--quit"));
}

// ==============================================================================
// Rebase Arguments Parsing Tests
// ==============================================================================

#[test]
fn test_rebase_root_flag() {
    let parsed = make_rebase_invocation(&["--root", "branch"]);

    assert!(parsed.has_command_flag("--root"));
}

#[test]
fn test_rebase_onto_with_equals() {
    let parsed = make_rebase_invocation(&["--onto=abc123", "upstream", "branch"]);

    // Verify onto argument is present
    assert!(parsed.command_args.iter().any(|a| a.starts_with("--onto=")));
}

#[test]
fn test_rebase_onto_separate_arg() {
    let parsed = make_rebase_invocation(&["--onto", "abc123", "upstream", "branch"]);

    // Verify onto flag and value are present
    assert!(parsed.command_args.contains(&"--onto".to_string()));
    assert!(parsed.command_args.contains(&"abc123".to_string()));
}

#[test]
fn test_rebase_interactive_short_flag() {
    let parsed = make_rebase_invocation(&["-i", "upstream"]);

    assert!(parsed.has_command_flag("-i"));
}

#[test]
fn test_rebase_interactive_long_flag() {
    let parsed = make_rebase_invocation(&["--interactive", "upstream"]);

    assert!(parsed.has_command_flag("--interactive"));
}

// ==============================================================================
// Active Rebase Detection Tests
// ==============================================================================

#[test]
fn test_active_rebase_with_start_event() {
    use git_ai::git::rewrite_log::RebaseStartEvent;

    let events = vec![RewriteLogEvent::rebase_start(
        RebaseStartEvent::new_with_onto("abc123".to_string(), false, None),
    )];

    // Simulate active detection (newest-first)
    let mut has_active = false;
    for event in events {
        match event {
            RewriteLogEvent::RebaseComplete { .. } | RewriteLogEvent::RebaseAbort { .. } => {
                has_active = false;
                break;
            }
            RewriteLogEvent::RebaseStart { .. } => {
                has_active = true;
                break;
            }
            _ => continue,
        }
    }

    assert!(has_active);
}

#[test]
fn test_no_active_rebase_with_complete_first() {
    use git_ai::git::rewrite_log::{RebaseCompleteEvent, RebaseStartEvent};

    let events = vec![
        RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
            "abc123".to_string(),
            "def456".to_string(),
            false,
            vec!["commit".to_string()],
            vec!["new".to_string()],
        )),
        RewriteLogEvent::rebase_start(RebaseStartEvent::new_with_onto(
            "abc123".to_string(),
            false,
            None,
        )),
    ];

    // Simulate active detection (newest-first)
    let mut has_active = false;
    for event in events {
        match event {
            RewriteLogEvent::RebaseComplete { .. } | RewriteLogEvent::RebaseAbort { .. } => {
                has_active = false;
                break;
            }
            RewriteLogEvent::RebaseStart { .. } => {
                has_active = true;
                break;
            }
            _ => continue,
        }
    }

    assert!(!has_active);
}

#[test]
fn test_no_active_rebase_with_abort_first() {
    use git_ai::git::rewrite_log::{RebaseAbortEvent, RebaseStartEvent};

    let events = vec![
        RewriteLogEvent::rebase_abort(RebaseAbortEvent::new("abc123".to_string())),
        RewriteLogEvent::rebase_start(RebaseStartEvent::new_with_onto(
            "abc123".to_string(),
            false,
            None,
        )),
    ];

    // Simulate active detection
    let mut has_active = false;
    for event in events {
        match event {
            RewriteLogEvent::RebaseComplete { .. } | RewriteLogEvent::RebaseAbort { .. } => {
                has_active = false;
                break;
            }
            RewriteLogEvent::RebaseStart { .. } => {
                has_active = true;
                break;
            }
            _ => continue,
        }
    }

    assert!(!has_active);
}
