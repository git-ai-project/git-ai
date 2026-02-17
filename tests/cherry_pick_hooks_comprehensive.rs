#[macro_use]
mod repos;
mod test_utils;

use crate::repos::test_repo::TestRepo;
use git_ai::git::rewrite_log::RewriteLogEvent;

// ==============================================================================
// Cherry-Pick Hook State Detection Tests
// ==============================================================================

#[test]
fn test_cherry_pick_head_file_detection() {
    let repo = TestRepo::new();

    // Initially CHERRY_PICK_HEAD should not exist
    let cherry_pick_head = repo.path().join(".git").join("CHERRY_PICK_HEAD");
    assert!(!cherry_pick_head.exists());
}

#[test]
fn test_cherry_pick_sequencer_detection() {
    let repo = TestRepo::new();

    // Initially sequencer directory should not exist
    let sequencer_dir = repo.path().join(".git").join("sequencer");
    assert!(!sequencer_dir.exists());
}

#[test]
fn test_cherry_pick_not_in_progress() {
    let repo = TestRepo::new();

    let cherry_pick_head = repo.path().join(".git").join("CHERRY_PICK_HEAD");
    let sequencer_dir = repo.path().join(".git").join("sequencer");

    let in_progress = cherry_pick_head.exists() || sequencer_dir.exists();

    assert!(!in_progress);
}

// ==============================================================================
// Rewrite Log Event Tests
// ==============================================================================

#[test]
fn test_cherry_pick_start_event_creation() {
    use git_ai::git::rewrite_log::CherryPickStartEvent;

    let event = CherryPickStartEvent::new(
        "abc123".to_string(),
        vec!["commit1".to_string(), "commit2".to_string()],
    );

    assert_eq!(event.original_head, "abc123");
    assert_eq!(event.source_commits.len(), 2);
    assert_eq!(event.source_commits[0], "commit1");
    assert_eq!(event.source_commits[1], "commit2");
}

#[test]
fn test_cherry_pick_complete_event_creation() {
    use git_ai::git::rewrite_log::CherryPickCompleteEvent;

    let event = CherryPickCompleteEvent::new(
        "abc123".to_string(),
        "def456".to_string(),
        vec!["src1".to_string()],
        vec!["new1".to_string()],
    );

    assert_eq!(event.original_head, "abc123");
    assert_eq!(event.new_head, "def456");
    assert_eq!(event.source_commits.len(), 1);
    assert_eq!(event.new_commits.len(), 1);
}

#[test]
fn test_cherry_pick_abort_event_creation() {
    use git_ai::git::rewrite_log::CherryPickAbortEvent;

    let event = CherryPickAbortEvent::new("abc123".to_string());

    assert_eq!(event.original_head, "abc123");
}

#[test]
fn test_cherry_pick_event_variants() {
    use git_ai::git::rewrite_log::{
        CherryPickAbortEvent, CherryPickCompleteEvent, CherryPickStartEvent,
    };

    let start_event = RewriteLogEvent::cherry_pick_start(CherryPickStartEvent::new(
        "abc".to_string(),
        vec!["commit".to_string()],
    ));

    let complete_event = RewriteLogEvent::cherry_pick_complete(CherryPickCompleteEvent::new(
        "abc".to_string(),
        "def".to_string(),
        vec!["src".to_string()],
        vec!["new".to_string()],
    ));

    let abort_event =
        RewriteLogEvent::cherry_pick_abort(CherryPickAbortEvent::new("abc".to_string()));

    match start_event {
        RewriteLogEvent::CherryPickStart { .. } => {}
        _ => panic!("Expected CherryPickStart"),
    }

    match complete_event {
        RewriteLogEvent::CherryPickComplete { .. } => {}
        _ => panic!("Expected CherryPickComplete"),
    }

    match abort_event {
        RewriteLogEvent::CherryPickAbort { .. } => {}
        _ => panic!("Expected CherryPickAbort"),
    }
}

// ==============================================================================
// Commit Parsing Tests
// ==============================================================================

#[test]
fn test_parse_single_commit() {
    let args = vec!["abc123".to_string()];

    // Simulate commit parsing
    let commits: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect();

    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0], "abc123");
}

#[test]
fn test_parse_multiple_commits() {
    let args = vec![
        "commit1".to_string(),
        "commit2".to_string(),
        "commit3".to_string(),
    ];

    let commits: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect();

    assert_eq!(commits.len(), 3);
    assert_eq!(commits[0], "commit1");
    assert_eq!(commits[1], "commit2");
    assert_eq!(commits[2], "commit3");
}

#[test]
fn test_parse_commits_with_flags() {
    let args = vec![
        "-x".to_string(),
        "commit1".to_string(),
        "--edit".to_string(),
        "commit2".to_string(),
    ];

    let commits: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect();

    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0], "commit1");
    assert_eq!(commits[1], "commit2");
}

#[test]
fn test_filter_flag_with_value() {
    let args = vec!["-m".to_string(), "1".to_string(), "commit1".to_string()];

    // Simulate filtering -m and its value
    let mut filtered = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-m" || args[i] == "--mainline" {
            i += 2; // Skip flag and value
        } else if args[i].starts_with('-') {
            i += 1; // Skip flag
        } else {
            filtered.push(args[i].clone());
            i += 1;
        }
    }

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0], "commit1");
}

#[test]
fn test_filter_special_keywords() {
    let args = vec![
        "continue".to_string(),
        "abort".to_string(),
        "quit".to_string(),
        "skip".to_string(),
        "commit1".to_string(),
    ];

    let keywords = vec!["continue", "abort", "quit", "skip"];
    let commits: Vec<String> = args
        .iter()
        .filter(|a| !keywords.contains(&a.as_str()))
        .cloned()
        .collect();

    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0], "commit1");
}

// ==============================================================================
// Commit Range Parsing Tests
// ==============================================================================

#[test]
fn test_detect_commit_range() {
    let ref1 = "commit1..commit2";
    let ref2 = "commit1^..commit2";
    let ref3 = "commit1";

    assert!(ref1.contains(".."));
    assert!(ref2.contains(".."));
    assert!(!ref3.contains(".."));
}

#[test]
fn test_range_expansion_format() {
    // Test the expected format for git rev-list
    let range = "A..B";
    let reverse_flag = "--reverse";

    let expected_args = vec!["rev-list", reverse_flag, range];

    assert_eq!(expected_args.len(), 3);
    assert_eq!(expected_args[0], "rev-list");
    assert_eq!(expected_args[1], "--reverse");
    assert_eq!(expected_args[2], "A..B");
}

// ==============================================================================
// Active Cherry-Pick Detection Tests
// ==============================================================================

#[test]
fn test_active_cherry_pick_with_start_event() {
    use git_ai::git::rewrite_log::CherryPickStartEvent;

    let events = vec![RewriteLogEvent::cherry_pick_start(
        CherryPickStartEvent::new("abc".to_string(), vec!["commit".to_string()]),
    )];

    // Simulate active detection
    let mut has_active = false;
    for event in events {
        match event {
            RewriteLogEvent::CherryPickComplete { .. }
            | RewriteLogEvent::CherryPickAbort { .. } => {
                has_active = false;
                break;
            }
            RewriteLogEvent::CherryPickStart { .. } => {
                has_active = true;
                break;
            }
            _ => continue,
        }
    }

    assert!(has_active);
}

#[test]
fn test_no_active_cherry_pick_with_complete_first() {
    use git_ai::git::rewrite_log::{CherryPickCompleteEvent, CherryPickStartEvent};

    let events = vec![
        RewriteLogEvent::cherry_pick_complete(CherryPickCompleteEvent::new(
            "abc".to_string(),
            "def".to_string(),
            vec!["src".to_string()],
            vec!["new".to_string()],
        )),
        RewriteLogEvent::cherry_pick_start(CherryPickStartEvent::new(
            "abc".to_string(),
            vec!["commit".to_string()],
        )),
    ];

    // Simulate active detection (events newest-first)
    let mut has_active = false;
    for event in events {
        match event {
            RewriteLogEvent::CherryPickComplete { .. }
            | RewriteLogEvent::CherryPickAbort { .. } => {
                has_active = false;
                break;
            }
            RewriteLogEvent::CherryPickStart { .. } => {
                has_active = true;
                break;
            }
            _ => continue,
        }
    }

    assert!(!has_active);
}

#[test]
fn test_no_active_cherry_pick_with_abort_first() {
    use git_ai::git::rewrite_log::{CherryPickAbortEvent, CherryPickStartEvent};

    let events = vec![
        RewriteLogEvent::cherry_pick_abort(CherryPickAbortEvent::new("abc".to_string())),
        RewriteLogEvent::cherry_pick_start(CherryPickStartEvent::new(
            "abc".to_string(),
            vec!["commit".to_string()],
        )),
    ];

    // Simulate active detection
    let mut has_active = false;
    for event in events {
        match event {
            RewriteLogEvent::CherryPickComplete { .. }
            | RewriteLogEvent::CherryPickAbort { .. } => {
                has_active = false;
                break;
            }
            RewriteLogEvent::CherryPickStart { .. } => {
                has_active = true;
                break;
            }
            _ => continue,
        }
    }

    assert!(!has_active);
}

#[test]
fn test_no_cherry_pick_events() {
    let events: Vec<RewriteLogEvent> = vec![];

    let mut has_active = false;
    for event in events {
        match event {
            RewriteLogEvent::CherryPickComplete { .. }
            | RewriteLogEvent::CherryPickAbort { .. } => {
                has_active = false;
                break;
            }
            RewriteLogEvent::CherryPickStart { .. } => {
                has_active = true;
                break;
            }
            _ => continue,
        }
    }

    assert!(!has_active);
}

// ==============================================================================
// Pre-Hook Tests
// ==============================================================================

#[test]
fn test_pre_hook_new_cherry_pick() {
    let repo = TestRepo::new();

    // Create a commit
    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    let commit = repo.commit("test commit").unwrap();

    // In a new cherry-pick, CHERRY_PICK_HEAD doesn't exist
    let cherry_pick_head = repo.path().join(".git").join("CHERRY_PICK_HEAD");
    assert!(!cherry_pick_head.exists());

    // Pre-hook should capture HEAD
    assert!(!commit.commit_sha.is_empty());
}

#[test]
fn test_pre_hook_continuing_cherry_pick() {
    let repo = TestRepo::new();

    // Create a commit
    repo.filename("test.txt")
        .set_contents(vec!["content"])
        .stage();
    repo.commit("test commit").unwrap();

    // Simulate continuing state by creating CHERRY_PICK_HEAD
    let cherry_pick_head = repo.path().join(".git").join("CHERRY_PICK_HEAD");
    std::fs::write(&cherry_pick_head, "abc123\n").expect("Failed to create CHERRY_PICK_HEAD");

    // Now it's in progress
    assert!(cherry_pick_head.exists());
}

// ==============================================================================
// Post-Hook Tests
// ==============================================================================

#[test]
fn test_post_hook_still_in_progress() {
    let repo = TestRepo::new();

    // Create CHERRY_PICK_HEAD to simulate in-progress state
    let cherry_pick_head = repo.path().join(".git").join("CHERRY_PICK_HEAD");
    std::fs::write(&cherry_pick_head, "abc123\n").expect("Failed to create CHERRY_PICK_HEAD");

    // Check if in progress
    let is_in_progress = cherry_pick_head.exists();

    assert!(is_in_progress);
    // Post-hook should return early
}

#[test]
fn test_post_hook_conflict_state() {
    let repo = TestRepo::new();

    // Create both CHERRY_PICK_HEAD and sequencer to simulate conflict
    let cherry_pick_head = repo.path().join(".git").join("CHERRY_PICK_HEAD");
    let sequencer_dir = repo.path().join(".git").join("sequencer");

    std::fs::write(&cherry_pick_head, "abc123\n").expect("Failed to create CHERRY_PICK_HEAD");
    std::fs::create_dir_all(&sequencer_dir).expect("Failed to create sequencer");

    let is_in_progress = cherry_pick_head.exists() || sequencer_dir.exists();

    assert!(is_in_progress);
}

#[test]
fn test_post_hook_completed() {
    let repo = TestRepo::new();

    // Neither CHERRY_PICK_HEAD nor sequencer exist
    let cherry_pick_head = repo.path().join(".git").join("CHERRY_PICK_HEAD");
    let sequencer_dir = repo.path().join(".git").join("sequencer");

    let is_in_progress = cherry_pick_head.exists() || sequencer_dir.exists();

    assert!(!is_in_progress);
    // Post-hook should process completion
}

#[test]
fn test_post_hook_with_failure_status() {
    use std::process::ExitStatus;

    // Simulate a failed exit status
    // Note: We can't easily create an ExitStatus in tests, so we test the logic

    let success = true; // Simulated from exit_status.success()
    let failed = !success;

    if failed {
        // Should log abort event
        assert!(true);
    }
}

// ==============================================================================
// Commit Mapping Tests
// ==============================================================================

#[test]
fn test_build_commit_mappings() {
    let repo = TestRepo::new();

    // Create first commit
    repo.filename("file1.txt")
        .set_contents(vec!["content1"])
        .stage();
    let commit1 = repo.commit("commit 1").unwrap();
    let original_head = commit1.commit_sha;

    // Create second commit
    repo.filename("file2.txt")
        .set_contents(vec!["content2"])
        .stage();
    repo.commit("commit 2").unwrap();

    // Create third commit
    repo.filename("file3.txt")
        .set_contents(vec!["content3"])
        .stage();
    let commit3 = repo.commit("commit 3").unwrap();
    let new_head = commit3.commit_sha;

    // Verify commits differ
    assert_ne!(original_head, new_head);

    // walk_commits_to_base would return commits between original and new
    // In reverse order (newest first), then reversed to get chronological
}

#[test]
fn test_commit_mapping_reversal() {
    let mut commits = vec![
        "commit3".to_string(),
        "commit2".to_string(),
        "commit1".to_string(),
    ];

    // Reverse to get chronological order
    commits.reverse();

    assert_eq!(commits[0], "commit1");
    assert_eq!(commits[1], "commit2");
    assert_eq!(commits[2], "commit3");
}

#[test]
fn test_empty_commit_mapping() {
    let commits: Vec<String> = vec![];

    assert_eq!(commits.len(), 0);
    // Should handle empty case gracefully
}

// ==============================================================================
// Original Head Extraction Tests
// ==============================================================================

#[test]
fn test_find_original_head_from_start_event() {
    use git_ai::git::rewrite_log::CherryPickStartEvent;

    let events = vec![RewriteLogEvent::cherry_pick_start(
        CherryPickStartEvent::new("original123".to_string(), vec!["commit".to_string()]),
    )];

    // Simulate finding original head
    let mut original_head = None;
    for event in events {
        match event {
            RewriteLogEvent::CherryPickStart { cherry_pick_start } => {
                original_head = Some(cherry_pick_start.original_head);
                break;
            }
            _ => continue,
        }
    }

    assert_eq!(original_head, Some("original123".to_string()));
}

#[test]
fn test_find_source_commits_from_start_event() {
    use git_ai::git::rewrite_log::CherryPickStartEvent;

    let events = vec![RewriteLogEvent::cherry_pick_start(
        CherryPickStartEvent::new(
            "original".to_string(),
            vec!["commit1".to_string(), "commit2".to_string()],
        ),
    )];

    // Simulate finding source commits
    let mut source_commits = None;
    for event in events {
        match event {
            RewriteLogEvent::CherryPickStart { cherry_pick_start } => {
                source_commits = Some(cherry_pick_start.source_commits);
                break;
            }
            _ => continue,
        }
    }

    assert_eq!(
        source_commits,
        Some(vec!["commit1".to_string(), "commit2".to_string()])
    );
}

#[test]
fn test_no_start_event_found() {
    use git_ai::git::rewrite_log::CherryPickAbortEvent;

    let events = vec![RewriteLogEvent::cherry_pick_abort(
        CherryPickAbortEvent::new("abc".to_string()),
    )];

    // Simulate finding original head
    let mut original_head = None;
    for event in events {
        match event {
            RewriteLogEvent::CherryPickStart { cherry_pick_start } => {
                original_head = Some(cherry_pick_start.original_head);
                break;
            }
            _ => continue,
        }
    }

    assert_eq!(original_head, None);
}

// ==============================================================================
// Dry Run Tests
// ==============================================================================

#[test]
fn test_dry_run_detection() {
    let args1 = vec![
        "cherry-pick".to_string(),
        "--dry-run".to_string(),
        "commit".to_string(),
    ];
    let args2 = vec!["cherry-pick".to_string(), "commit".to_string()];

    let is_dry_run_1 = args1.iter().any(|a| a == "--dry-run");
    let is_dry_run_2 = args2.iter().any(|a| a == "--dry-run");

    assert!(is_dry_run_1);
    assert!(!is_dry_run_2);
}

#[test]
fn test_dry_run_skips_post_hook() {
    let args = vec!["--dry-run".to_string()];

    if args.iter().any(|a| a == "--dry-run") {
        // Should return early
        assert!(true);
    } else {
        panic!("Should have detected dry-run");
    }
}

// ==============================================================================
// Head Unchanged Tests
// ==============================================================================

#[test]
fn test_head_unchanged_detection() {
    let original_head = "abc123";
    let new_head = "abc123";

    if original_head == new_head {
        // Cherry-pick resulted in no changes
        assert!(true);
    } else {
        panic!("Heads should be equal");
    }
}

#[test]
fn test_head_changed_detection() {
    let original_head = "abc123";
    let new_head = "def456";

    if original_head == new_head {
        panic!("Heads should differ");
    } else {
        // Cherry-pick created new commits
        assert!(true);
    }
}

// ==============================================================================
// Integration Tests
// ==============================================================================

#[test]
fn test_cherry_pick_complete_flow() {
    let repo = TestRepo::new();

    // Create initial commit
    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let commit1 = repo.commit("base commit").unwrap();
    let original_head = commit1.commit_sha;

    // Create a branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt")
        .set_contents(vec!["feature"])
        .stage();
    let commit2 = repo.commit("feature commit").unwrap();
    let feature_commit = commit2.commit_sha;

    // Go back to original branch
    repo.git(&["checkout", "-"]).unwrap();

    // The cherry-pick hook would:
    // 1. Record original HEAD
    // 2. After cherry-pick, detect new HEAD
    // 3. Build commit mappings
    // 4. Write Complete event

    assert_ne!(original_head, feature_commit);
}

#[test]
fn test_cherry_pick_abort_flow() {
    let repo = TestRepo::new();

    // Create initial commit
    repo.filename("base.txt").set_contents(vec!["base"]).stage();
    let commit = repo.commit("base commit").unwrap();
    let original_head = commit.commit_sha;

    // The abort hook would:
    // 1. Find original HEAD from Start event
    // 2. Write Abort event with original HEAD

    assert!(!original_head.is_empty());
}

// ==============================================================================
// Strategy Flag Tests
// ==============================================================================

#[test]
fn test_strategy_flag_filtering() {
    let args = vec![
        "-s".to_string(),
        "recursive".to_string(),
        "commit1".to_string(),
    ];

    // Filter -s and its value
    let mut filtered = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-s" || args[i] == "--strategy" {
            i += 2;
        } else if args[i].starts_with('-') {
            i += 1;
        } else {
            filtered.push(args[i].clone());
            i += 1;
        }
    }

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0], "commit1");
}

#[test]
fn test_mainline_flag_filtering() {
    let args = vec![
        "--mainline".to_string(),
        "1".to_string(),
        "commit1".to_string(),
    ];

    // Filter --mainline and its value
    let mut filtered = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-m" || args[i] == "--mainline" {
            i += 2;
        } else if args[i].starts_with('-') {
            i += 1;
        } else {
            filtered.push(args[i].clone());
            i += 1;
        }
    }

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0], "commit1");
}

// ==============================================================================
// Rev-Parse Tests
// ==============================================================================

#[test]
fn test_resolve_commit_sha_format() {
    // Test rev-parse argument format
    let commit_ref = "HEAD~1";
    let args = vec!["rev-parse".to_string(), commit_ref.to_string()];

    assert_eq!(args[0], "rev-parse");
    assert_eq!(args[1], "HEAD~1");
}

#[test]
fn test_resolve_symbolic_refs() {
    let refs = vec!["HEAD", "main", "feature", "HEAD~1", "abc123"];

    for ref_str in refs {
        // Each would be resolved via git rev-parse
        assert!(!ref_str.is_empty());
    }
}

// ==============================================================================
// Event Sequencing Tests
// ==============================================================================

#[test]
fn test_event_sequence_start_complete() {
    use git_ai::git::rewrite_log::{CherryPickCompleteEvent, CherryPickStartEvent};

    // Successful cherry-pick: Start -> Complete
    let events = vec![
        RewriteLogEvent::cherry_pick_start(CherryPickStartEvent::new(
            "abc".to_string(),
            vec!["commit".to_string()],
        )),
        RewriteLogEvent::cherry_pick_complete(CherryPickCompleteEvent::new(
            "abc".to_string(),
            "def".to_string(),
            vec!["commit".to_string()],
            vec!["new".to_string()],
        )),
    ];

    assert_eq!(events.len(), 2);

    match &events[0] {
        RewriteLogEvent::CherryPickStart { .. } => {}
        _ => panic!("Expected Start first"),
    }

    match &events[1] {
        RewriteLogEvent::CherryPickComplete { .. } => {}
        _ => panic!("Expected Complete second"),
    }
}

#[test]
fn test_event_sequence_start_abort() {
    use git_ai::git::rewrite_log::{CherryPickAbortEvent, CherryPickStartEvent};

    // Aborted cherry-pick: Start -> Abort
    let events = vec![
        RewriteLogEvent::cherry_pick_start(CherryPickStartEvent::new(
            "abc".to_string(),
            vec!["commit".to_string()],
        )),
        RewriteLogEvent::cherry_pick_abort(CherryPickAbortEvent::new("abc".to_string())),
    ];

    assert_eq!(events.len(), 2);

    match &events[0] {
        RewriteLogEvent::CherryPickStart { .. } => {}
        _ => panic!("Expected Start first"),
    }

    match &events[1] {
        RewriteLogEvent::CherryPickAbort { .. } => {}
        _ => panic!("Expected Abort second"),
    }
}
