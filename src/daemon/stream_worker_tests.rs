use super::stream_worker::{Priority, ProcessingTask, session_repo_allowed};
use crate::config::Config;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Initialize a git repo in `dir` with an `origin` remote pointing at `remote_url`.
fn init_repo_with_remote(dir: &Path, remote_url: &str) {
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git command failed to spawn");
        assert!(status.success(), "git {:?} failed", args);
    };
    run(&["init", "-q"]);
    run(&["remote", "add", "origin", remote_url]);
}

#[test]
fn test_session_repo_allowed_no_filters_allows_everything() {
    // With no allow/exclude filters configured, everything is allowed even
    // when the work_dir is unknown (the common, fast-path case).
    let config = Config::with_repository_filters_for_test(&[], &[]);
    assert!(session_repo_allowed(&config, None));
    assert!(session_repo_allowed(
        &config,
        Some(Path::new("/nonexistent"))
    ));
}

#[test]
fn test_session_repo_allowed_allowlist_matches() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_remote(dir.path(), "git@github.com:acme/app.git");

    let config = Config::with_repository_filters_for_test(&["*github.com/acme/*"], &[]);
    assert!(
        session_repo_allowed(&config, Some(dir.path())),
        "session in an allowlisted repo must be allowed"
    );
}

#[test]
fn test_session_repo_allowed_allowlist_excludes_non_matching() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_remote(dir.path(), "git@github.com:other/app.git");

    let config = Config::with_repository_filters_for_test(&["*github.com/acme/*"], &[]);
    assert!(
        !session_repo_allowed(&config, Some(dir.path())),
        "session in a repo outside the allowlist must be dropped"
    );
}

#[test]
fn test_session_repo_allowed_exclude_takes_precedence() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_remote(dir.path(), "git@github.com:acme/secret.git");

    let config = Config::with_repository_filters_for_test(&[], &["*github.com/acme/secret*"]);
    assert!(
        !session_repo_allowed(&config, Some(dir.path())),
        "session in an excluded repo must be dropped"
    );
}

#[test]
fn test_session_repo_allowed_fails_closed_when_repo_unknown_under_allowlist() {
    // An active allowlist plus an undeterminable repository (shared streams,
    // agents without cwd, or a path that isn't a git repo) must fail closed:
    // customers set allowlists for security, so unverifiable data is dropped.
    let config = Config::with_repository_filters_for_test(&["*github.com/acme/*"], &[]);
    assert!(
        !session_repo_allowed(&config, None),
        "no work_dir under an active allowlist must be dropped"
    );
    assert!(
        !session_repo_allowed(&config, Some(Path::new("/definitely/not/a/repo"))),
        "non-repo path under an active allowlist must be dropped"
    );
}

#[test]
fn test_session_repo_allowed_exclude_only_passes_unknown_repo() {
    // With only an exclude list (no allowlist), an undeterminable repository
    // can't match any exclude pattern, so it is allowed through. This keeps
    // shared streams (e.g. Copilot OTEL) flowing unless an allowlist is set.
    let config = Config::with_repository_filters_for_test(&[], &["*github.com/acme/secret*"]);
    assert!(
        session_repo_allowed(&config, None),
        "unknown repo with exclude-only filters must be allowed"
    );
}

#[test]
fn test_priority_queue_ordering_immediate_first() {
    let mut heap = BinaryHeap::new();

    // Insert tasks in reverse priority order
    heap.push(ProcessingTask {
        session_id: "low".to_string(),
        stream_kind: "transcript".to_string(),
        priority: Priority::Low,
        tool: "test".to_string(),
        trace_id: None,
        tool_use_id: None,
        canonical_path: PathBuf::from("/test"),
        repo_work_dir: None,
        retry_count: 0,
        next_retry_at: None,
    });
    heap.push(ProcessingTask {
        session_id: "immediate".to_string(),
        stream_kind: "transcript".to_string(),
        priority: Priority::Immediate,
        tool: "test".to_string(),
        trace_id: None,
        tool_use_id: None,
        canonical_path: PathBuf::from("/test"),
        repo_work_dir: None,
        retry_count: 0,
        next_retry_at: None,
    });

    // pop() should return Immediate first, then High, then Low
    let first = heap.pop().unwrap();
    assert_eq!(
        first.priority,
        Priority::Immediate,
        "Immediate priority should be popped first"
    );
    assert_eq!(first.session_id, "immediate");

    let second = heap.pop().unwrap();
    assert_eq!(
        second.priority,
        Priority::Low,
        "Low priority should be popped last"
    );
    assert_eq!(second.session_id, "low");
}

#[test]
fn test_priority_queue_ordering_multiple_same_priority() {
    let mut heap = BinaryHeap::new();

    heap.push(ProcessingTask {
        session_id: "immediate-2".to_string(),
        stream_kind: "transcript".to_string(),
        priority: Priority::Immediate,
        tool: "test".to_string(),
        trace_id: None,
        tool_use_id: None,
        canonical_path: PathBuf::from("/test"),
        repo_work_dir: None,
        retry_count: 0,
        next_retry_at: None,
    });
    heap.push(ProcessingTask {
        session_id: "low-1".to_string(),
        stream_kind: "transcript".to_string(),
        priority: Priority::Low,
        tool: "test".to_string(),
        trace_id: None,
        tool_use_id: None,
        canonical_path: PathBuf::from("/test"),
        repo_work_dir: None,
        retry_count: 0,
        next_retry_at: None,
    });
    heap.push(ProcessingTask {
        session_id: "immediate-1".to_string(),
        stream_kind: "transcript".to_string(),
        priority: Priority::Immediate,
        tool: "test".to_string(),
        trace_id: None,
        tool_use_id: None,
        canonical_path: PathBuf::from("/test"),
        repo_work_dir: None,
        retry_count: 0,
        next_retry_at: None,
    });

    // Both immediate tasks should come out before low
    let first = heap.pop().unwrap();
    assert_eq!(first.priority, Priority::Immediate);

    let second = heap.pop().unwrap();
    assert_eq!(second.priority, Priority::Immediate);

    let third = heap.pop().unwrap();
    assert_eq!(third.priority, Priority::Low);
}

#[test]
fn test_retry_delay_prevents_immediate_reprocessing() {
    let mut heap = BinaryHeap::new();

    // Create a task with retry scheduled for 5 seconds in the future
    let now = Instant::now();
    let next_retry_at = now + Duration::from_secs(5);

    let task = ProcessingTask {
        session_id: "retry-test".to_string(),
        stream_kind: "transcript".to_string(),
        priority: Priority::Immediate,
        tool: "test".to_string(),
        trace_id: None,
        tool_use_id: None,
        canonical_path: PathBuf::from("/test"),
        repo_work_dir: None,
        retry_count: 1,
        next_retry_at: Some(next_retry_at),
    };

    heap.push(task.clone());

    // Task should pop from heap
    let popped = heap.pop().unwrap();
    assert_eq!(popped.session_id, "retry-test");

    // But it should NOT be processable until next_retry_at has passed
    assert!(popped.next_retry_at.is_some());
    assert!(
        popped.next_retry_at.unwrap() > now,
        "Task should have a future retry time"
    );

    // Simulating the check: is it time to process?
    let ready_to_process = popped
        .next_retry_at
        .map(|retry_at| Instant::now() >= retry_at)
        .unwrap_or(true);

    assert!(
        !ready_to_process,
        "Task should not be ready for immediate processing"
    );
}

#[test]
fn test_retry_delay_allows_processing_after_delay() {
    let now = Instant::now();

    // Create a task with retry scheduled for the past (simulating time has passed)
    let past_retry_at = now - Duration::from_secs(1);

    let task = ProcessingTask {
        session_id: "retry-past".to_string(),
        stream_kind: "transcript".to_string(),
        priority: Priority::Immediate,
        tool: "test".to_string(),
        trace_id: None,
        tool_use_id: None,
        canonical_path: PathBuf::from("/test"),
        repo_work_dir: None,
        retry_count: 1,
        next_retry_at: Some(past_retry_at),
    };

    // Check if ready to process
    let ready_to_process = task
        .next_retry_at
        .map(|retry_at| Instant::now() >= retry_at)
        .unwrap_or(true);

    assert!(
        ready_to_process,
        "Task with past retry time should be ready for processing"
    );
}
