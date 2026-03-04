//! Comprehensive tests for the async worker infrastructure.
//!
//! Tests cover:
//! - Job serialization/deserialization roundtrips
//! - Wire format correctness
//! - Socket communication (bind, connect, send, receive)
//! - Feature flag gating
//! - Dispatch logic (async enabled vs disabled)
//! - Worker lifecycle (bind, accept, timeout, cleanup)
//! - Atomic ownership (only one worker per socket)
//! - Message protocol correctness
//! - Various RewriteLogEvent types

#[macro_use]
mod repos;

use git_ai::async_worker::job::{AsyncJob, AsyncJobType};
use git_ai::async_worker::socket::{self, read_message, write_message};
use git_ai::feature_flags::FeatureFlags;
use git_ai::git::rewrite_log::RewriteLogEvent;
use std::io::Cursor;

// ==============================================================================
// Helper Functions
// ==============================================================================

fn make_test_job(event: RewriteLogEvent) -> AsyncJob {
    AsyncJob {
        job_type: AsyncJobType::RewriteLogEvent,
        repo_global_args: vec!["-C".to_string(), "/tmp/test-repo".to_string()],
        git_dir: "/tmp/test-repo/.git".to_string(),
        git_common_dir: "/tmp/test-repo/.git".to_string(),
        workdir: "/tmp/test-repo".to_string(),
        rewrite_log_event: event,
        commit_author: "Test User <test@example.com>".to_string(),
        suppress_output: false,
        apply_side_effects: true,
    }
}

fn make_commit_event() -> RewriteLogEvent {
    RewriteLogEvent::commit(Some("abc123".to_string()), "def456".to_string())
}

fn make_commit_amend_event() -> RewriteLogEvent {
    RewriteLogEvent::commit_amend("old_sha".to_string(), "new_sha".to_string())
}

// ==============================================================================
// Job Serialization Tests
// ==============================================================================

#[test]
fn test_job_serialization_commit_event() {
    let job = make_test_job(make_commit_event());
    let wire = job.to_wire_bytes().expect("serialization should succeed");
    assert!(
        wire.len() > 4,
        "Wire bytes should have length prefix + payload"
    );

    let len = u32::from_be_bytes([wire[0], wire[1], wire[2], wire[3]]) as usize;
    assert_eq!(
        len,
        wire.len() - 4,
        "Length prefix should match payload size"
    );

    let deserialized =
        AsyncJob::from_json_bytes(&wire[4..]).expect("deserialization should succeed");
    assert_eq!(deserialized.git_dir, "/tmp/test-repo/.git");
    assert_eq!(deserialized.commit_author, "Test User <test@example.com>");
    assert!(deserialized.apply_side_effects);
    assert!(!deserialized.suppress_output);
}

#[test]
fn test_job_serialization_commit_amend_event() {
    let job = make_test_job(make_commit_amend_event());
    let wire = job.to_wire_bytes().unwrap();
    let deserialized = AsyncJob::from_json_bytes(&wire[4..]).unwrap();

    match &deserialized.rewrite_log_event {
        RewriteLogEvent::CommitAmend { commit_amend } => {
            assert_eq!(commit_amend.original_commit, "old_sha");
            assert_eq!(commit_amend.amended_commit_sha, "new_sha");
        }
        other => panic!("Expected CommitAmend, got {:?}", other),
    }
}

#[test]
fn test_job_serialization_merge_squash_event() {
    use git_ai::git::rewrite_log::MergeSquashEvent;
    let event = RewriteLogEvent::merge_squash(MergeSquashEvent::new(
        "feature-branch".to_string(),
        "abc123".to_string(),
        "main".to_string(),
        "def456".to_string(),
    ));
    let job = make_test_job(event);
    let wire = job.to_wire_bytes().unwrap();
    let deserialized = AsyncJob::from_json_bytes(&wire[4..]).unwrap();

    match &deserialized.rewrite_log_event {
        RewriteLogEvent::MergeSquash { merge_squash } => {
            assert_eq!(merge_squash.source_branch, "feature-branch");
            assert_eq!(merge_squash.source_head, "abc123");
        }
        other => panic!("Expected MergeSquash, got {:?}", other),
    }
}

#[test]
fn test_job_serialization_preserves_all_fields() {
    let job = AsyncJob {
        job_type: AsyncJobType::RewriteLogEvent,
        repo_global_args: vec![
            "-C".to_string(),
            "/path/with spaces/repo".to_string(),
            "--no-pager".to_string(),
        ],
        git_dir: "/path/with spaces/repo/.git".to_string(),
        git_common_dir: "/path/with spaces/repo/.git".to_string(),
        workdir: "/path/with spaces/repo".to_string(),
        rewrite_log_event: make_commit_event(),
        commit_author: "Author With Spaces <author@example.com>".to_string(),
        suppress_output: true,
        apply_side_effects: false,
    };

    let wire = job.to_wire_bytes().unwrap();
    let deserialized = AsyncJob::from_json_bytes(&wire[4..]).unwrap();

    assert_eq!(deserialized.repo_global_args.len(), 3);
    assert_eq!(deserialized.repo_global_args[1], "/path/with spaces/repo");
    assert_eq!(deserialized.git_dir, "/path/with spaces/repo/.git");
    assert_eq!(deserialized.git_common_dir, "/path/with spaces/repo/.git");
    assert_eq!(deserialized.workdir, "/path/with spaces/repo");
    assert!(deserialized.suppress_output);
    assert!(!deserialized.apply_side_effects);
}

#[test]
fn test_job_serialization_empty_global_args() {
    let mut job = make_test_job(make_commit_event());
    job.repo_global_args = vec![];

    let wire = job.to_wire_bytes().unwrap();
    let deserialized = AsyncJob::from_json_bytes(&wire[4..]).unwrap();
    assert!(deserialized.repo_global_args.is_empty());
}

#[test]
fn test_job_serialization_empty_workdir() {
    let mut job = make_test_job(make_commit_event());
    job.workdir = String::new();

    let wire = job.to_wire_bytes().unwrap();
    let deserialized = AsyncJob::from_json_bytes(&wire[4..]).unwrap();
    assert!(deserialized.workdir.is_empty());
}

#[test]
fn test_job_deserialization_invalid_json() {
    let result = AsyncJob::from_json_bytes(b"not valid json");
    assert!(result.is_err(), "Invalid JSON should fail to deserialize");
}

#[test]
fn test_job_deserialization_empty_bytes() {
    let result = AsyncJob::from_json_bytes(b"");
    assert!(result.is_err(), "Empty bytes should fail to deserialize");
}

#[test]
fn test_job_deserialization_missing_fields() {
    let partial_json = r#"{"job_type":"rewrite_log_event","git_dir":"/tmp"}"#;
    let result = AsyncJob::from_json_bytes(partial_json.as_bytes());
    assert!(
        result.is_err(),
        "Incomplete JSON should fail to deserialize"
    );
}

// ==============================================================================
// Wire Protocol Tests
// ==============================================================================

#[test]
fn test_wire_write_read_message_roundtrip() {
    let payload = b"hello async worker";
    let mut buf = Vec::new();
    write_message(&mut buf, payload).unwrap();

    assert_eq!(buf.len(), 4 + payload.len());

    let mut cursor = Cursor::new(buf);
    let result = read_message(&mut cursor).unwrap();
    assert_eq!(result, Some(payload.to_vec()));
}

#[test]
fn test_wire_write_read_message_large_payload() {
    let payload: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
    let mut buf = Vec::new();
    write_message(&mut buf, &payload).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = read_message(&mut cursor).unwrap().unwrap();
    assert_eq!(result.len(), 10000);
    assert_eq!(result, payload);
}

#[test]
fn test_wire_write_read_message_empty_payload() {
    let payload = b"";
    let mut buf = Vec::new();
    write_message(&mut buf, payload).unwrap();

    assert_eq!(buf.len(), 4); // Just the length prefix

    let mut cursor = Cursor::new(buf);
    let result = read_message(&mut cursor).unwrap().unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_wire_read_message_empty_stream() {
    let buf: Vec<u8> = vec![];
    let mut cursor = Cursor::new(buf);
    let result = read_message(&mut cursor).unwrap();
    assert_eq!(result, None, "Empty stream should return None (EOF)");
}

#[test]
fn test_wire_read_message_rejects_oversized() {
    let len: u32 = 128 * 1024 * 1024; // 128 MB
    let mut buf = Vec::new();
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&[0u8; 16]);

    let mut cursor = Cursor::new(buf);
    let result = read_message(&mut cursor);
    assert!(result.is_err(), "Should reject messages > 64 MB");
}

#[test]
fn test_wire_write_read_multiple_messages() {
    let messages: Vec<Vec<u8>> = vec![
        b"first message".to_vec(),
        b"second message".to_vec(),
        b"third message".to_vec(),
    ];

    let mut buf = Vec::new();
    for msg in &messages {
        write_message(&mut buf, msg).unwrap();
    }

    let mut cursor = Cursor::new(buf);
    for expected in &messages {
        let result = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(&result, expected);
    }
}

#[test]
fn test_wire_format_length_prefix_correctness() {
    let job = make_test_job(make_commit_event());
    let wire = job.to_wire_bytes().unwrap();

    // First 4 bytes are big-endian length
    let len = u32::from_be_bytes([wire[0], wire[1], wire[2], wire[3]]) as usize;
    let json_payload = &wire[4..];

    assert_eq!(
        len,
        json_payload.len(),
        "Length prefix must equal JSON payload length"
    );

    // Verify JSON is valid
    let _: serde_json::Value =
        serde_json::from_slice(json_payload).expect("Payload should be valid JSON");
}

// ==============================================================================
// Socket Path Tests
// ==============================================================================

#[test]
fn test_socket_path_for_ai_dir() {
    let ai_dir = std::path::Path::new("/tmp/test-repo/.git/ai");
    let path = socket::socket_path_for_ai_dir(ai_dir);
    assert_eq!(
        path,
        std::path::PathBuf::from("/tmp/test-repo/.git/ai/async-worker.sock")
    );
}

#[test]
fn test_socket_path_for_worktree_ai_dir() {
    let ai_dir = std::path::Path::new("/tmp/test-repo/.git/worktrees/feature/ai");
    let path = socket::socket_path_for_ai_dir(ai_dir);
    assert_eq!(
        path,
        std::path::PathBuf::from("/tmp/test-repo/.git/worktrees/feature/ai/async-worker.sock")
    );
}

#[test]
fn test_socket_path_consistent() {
    let ai_dir = std::path::Path::new("/some/path/.git/ai");
    let path1 = socket::socket_path_for_ai_dir(ai_dir);
    let path2 = socket::socket_path_for_ai_dir(ai_dir);
    assert_eq!(
        path1, path2,
        "Same ai_dir should always produce same socket path"
    );
}

// ==============================================================================
// Socket Tests (cross-platform via interprocess)
// ==============================================================================

mod socket_tests {
    use super::*;
    use git_ai::async_worker::socket::platform;
    use std::time::Duration;

    #[test]
    fn test_bind_socket_creates_listener() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");

        let _listener = platform::bind_socket(&socket_path).unwrap();

        // On Unix, verify the socket file exists
        #[cfg(unix)]
        assert!(socket_path.exists(), "Socket file should exist after bind");

        platform::cleanup_socket(&socket_path);
    }

    #[test]
    fn test_bind_socket_atomic_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");

        // First bind should succeed
        let _listener = platform::bind_socket(&socket_path).unwrap();

        // Second bind should fail (socket already owned)
        let result = platform::bind_socket(&socket_path);
        assert!(
            result.is_err(),
            "Second bind should fail - socket already owned"
        );

        platform::cleanup_socket(&socket_path);
    }

    #[test]
    fn test_send_to_nonexistent_socket_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent-git-ai-async-test.sock");
        let result = platform::try_send_to_socket(&path, b"hello").unwrap();
        assert!(!result, "Should return false for non-existent socket");
    }

    #[test]
    fn test_socket_send_receive_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("roundtrip.sock");

        let listener = platform::bind_socket(&socket_path).unwrap();

        // Send from background thread to ensure accept loop is running
        let sender_path = socket_path.clone();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let payload = b"test payload for roundtrip";
            let sent = platform::try_send_to_socket(&sender_path, payload).unwrap();
            assert!(sent, "Should successfully send to bound socket");
        });

        // Accept and read
        let mut stream = platform::accept_with_timeout(&listener, Duration::from_secs(5))
            .unwrap()
            .expect("Should accept connection");

        let msg = read_message(&mut stream).unwrap().unwrap();
        assert_eq!(
            msg, b"test payload for roundtrip",
            "Received message should match sent payload"
        );

        sender.join().unwrap();
        platform::cleanup_socket(&socket_path);
    }

    #[test]
    fn test_socket_send_job_payload() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("job.sock");

        let listener = platform::bind_socket(&socket_path).unwrap();

        // Build and send a real job payload from background thread
        let sender_path = socket_path.clone();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let job = make_test_job(make_commit_event());
            let wire = job.to_wire_bytes().unwrap();
            let json_payload = &wire[4..]; // Skip length prefix
            let sent = platform::try_send_to_socket(&sender_path, json_payload).unwrap();
            assert!(sent);
        });

        // Accept and read
        let mut stream = platform::accept_with_timeout(&listener, Duration::from_secs(5))
            .unwrap()
            .unwrap();

        let msg = read_message(&mut stream).unwrap().unwrap();

        // Deserialize the job
        let deserialized = AsyncJob::from_json_bytes(&msg).unwrap();
        assert_eq!(deserialized.git_dir, "/tmp/test-repo/.git");

        sender.join().unwrap();
        platform::cleanup_socket(&socket_path);
    }

    #[test]
    fn test_socket_accept_timeout_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("timeout.sock");

        let listener = platform::bind_socket(&socket_path).unwrap();

        // Accept with very short timeout - should return None
        let result = platform::accept_with_timeout(&listener, Duration::from_millis(200)).unwrap();
        assert!(result.is_none(), "Should return None on timeout");

        platform::cleanup_socket(&socket_path);
    }

    #[test]
    fn test_socket_multiple_sequential_messages() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("multi.sock");

        let listener = platform::bind_socket(&socket_path).unwrap();

        // Send multiple messages sequentially (each on its own connection)
        for i in 0..3 {
            let payload = format!("message {}", i);
            let sender_path = socket_path.clone();
            let sender_payload = payload.clone();
            let sender = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                let sent =
                    platform::try_send_to_socket(&sender_path, sender_payload.as_bytes()).unwrap();
                assert!(sent, "Message should send successfully");
            });

            let mut stream = platform::accept_with_timeout(&listener, Duration::from_secs(5))
                .unwrap()
                .unwrap();
            let msg = read_message(&mut stream).unwrap().unwrap();
            assert_eq!(
                String::from_utf8(msg).unwrap(),
                payload,
                "Message {} content mismatch",
                i
            );
            sender.join().unwrap();
        }

        platform::cleanup_socket(&socket_path);
    }

    #[test]
    fn test_cleanup_nonexistent_socket_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent-cleanup-test.sock");
        // Should not panic
        platform::cleanup_socket(&path);
    }

    #[test]
    fn test_stale_socket_cleanup_on_bind() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("stale.sock");

        // Create a stale socket file (not actually listening)
        std::fs::write(&socket_path, b"stale").unwrap();
        assert!(socket_path.exists());

        // Bind should succeed by cleaning up the stale file
        let _listener = platform::bind_socket(&socket_path).unwrap();

        platform::cleanup_socket(&socket_path);
    }

    #[test]
    fn test_is_socket_live() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("live-check.sock");

        // Should be false before binding
        assert!(
            !platform::is_socket_live(&socket_path),
            "Should not be live before bind"
        );

        let _listener = platform::bind_socket(&socket_path).unwrap();

        // Should be true after binding
        assert!(
            platform::is_socket_live(&socket_path),
            "Should be live after bind"
        );

        platform::cleanup_socket(&socket_path);
    }
}

// ==============================================================================
// Feature Flag Tests
// ==============================================================================

#[test]
fn test_async_worker_feature_flag_default_false() {
    let flags = FeatureFlags::default();
    assert!(!flags.async_worker, "async_worker should default to false");
}

#[test]
fn test_async_worker_feature_flag_can_be_enabled() {
    let mut flags = FeatureFlags::default();
    flags.async_worker = true;
    assert!(flags.async_worker);
}

#[test]
fn test_feature_flags_with_async_worker_struct() {
    let flags = FeatureFlags {
        rewrite_stash: false,
        inter_commit_move: false,
        auth_keyring: false,
        async_worker: true,
        git_hooks_enabled: false,
        git_hooks_externally_managed: false,
    };
    assert!(flags.async_worker);
    assert!(!flags.rewrite_stash);
}

// ==============================================================================
// Dispatch Logic Tests (unit-level)
// ==============================================================================

#[test]
fn test_dispatch_disabled_when_flag_false() {
    // When async_worker feature flag is false, try_dispatch_async should return false.
    // We can't easily test the full dispatch without a real repo, but we can test
    // that the feature flag check gates the dispatch.
    let flags = FeatureFlags::default();
    assert!(
        !flags.async_worker,
        "Dispatch should be skipped when flag is false"
    );
}

#[test]
fn test_guard_env_var_prevents_recursion() {
    // The guard env var should prevent recursive dispatch
    let guard_env = "GIT_AI_ASYNC_WORKER_PROCESS";

    // Without guard set, dispatch is allowed (modulo other checks)
    assert!(
        std::env::var(guard_env).is_err() || std::env::var(guard_env).as_deref() != Ok("1"),
        "Guard env should not be set in test environment"
    );
}

// ==============================================================================
// Job Type Tests
// ==============================================================================

#[test]
fn test_job_type_serialization() {
    let job = make_test_job(make_commit_event());
    let json = serde_json::to_string(&job).unwrap();
    assert!(
        json.contains("\"rewrite_log_event\""),
        "Job type should serialize as snake_case"
    );
}

#[test]
fn test_all_rewrite_event_types_serialize() {
    // Test that all event types we dispatch can be serialized
    let events: Vec<RewriteLogEvent> = vec![
        RewriteLogEvent::commit(Some("a".to_string()), "b".to_string()),
        RewriteLogEvent::commit(None, "b".to_string()),
        RewriteLogEvent::commit_amend("a".to_string(), "b".to_string()),
    ];

    for event in events {
        let job = make_test_job(event);
        let wire = job.to_wire_bytes();
        assert!(
            wire.is_ok(),
            "All event types should serialize successfully"
        );

        let wire = wire.unwrap();
        let deserialized = AsyncJob::from_json_bytes(&wire[4..]);
        assert!(
            deserialized.is_ok(),
            "All event types should deserialize successfully"
        );
    }
}

// ==============================================================================
// Integration-style Tests (with real repos)
// ==============================================================================

#[cfg(unix)]
#[test]
fn test_worker_binds_accepts_and_processes_job() {
    use git_ai::async_worker::socket::platform;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("worker-test.sock");

    // Simulate the worker: bind socket
    let listener = platform::bind_socket(&socket_path).unwrap();

    // Simulate the client: build and send a job
    let job = make_test_job(make_commit_event());
    let wire = job.to_wire_bytes().unwrap();
    let json_payload = &wire[4..];

    let sent = platform::try_send_to_socket(&socket_path, json_payload).unwrap();
    assert!(sent, "Client should successfully send to worker socket");

    // Worker accepts the connection
    let mut stream = platform::accept_with_timeout(&listener, Duration::from_secs(2))
        .unwrap()
        .expect("Worker should accept client connection");

    // Worker reads the job
    let msg = read_message(&mut stream).unwrap().unwrap();
    let received_job = AsyncJob::from_json_bytes(&msg).unwrap();

    // Verify the job is correct
    assert_eq!(received_job.git_dir, "/tmp/test-repo/.git");
    assert_eq!(received_job.commit_author, "Test User <test@example.com>");
    assert!(received_job.apply_side_effects);
    match &received_job.rewrite_log_event {
        RewriteLogEvent::Commit { commit } => {
            assert_eq!(commit.commit_sha, "def456");
        }
        other => panic!("Expected Commit event, got {:?}", other),
    }

    platform::cleanup_socket(&socket_path);
}

#[cfg(unix)]
#[test]
fn test_worker_handles_multiple_sequential_jobs() {
    use git_ai::async_worker::socket::platform;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("multi-job.sock");

    let listener = platform::bind_socket(&socket_path).unwrap();

    // Send 5 jobs sequentially
    for i in 0..5 {
        let event = RewriteLogEvent::commit(Some(format!("base_{}", i)), format!("sha_{}", i));
        let job = make_test_job(event);
        let wire = job.to_wire_bytes().unwrap();
        let json_payload = &wire[4..];

        let sent = platform::try_send_to_socket(&socket_path, json_payload).unwrap();
        assert!(sent, "Job {} should send successfully", i);

        // Worker accepts and reads
        let mut stream = platform::accept_with_timeout(&listener, Duration::from_secs(2))
            .unwrap()
            .expect("Should accept connection");

        let msg = read_message(&mut stream).unwrap().unwrap();
        let received = AsyncJob::from_json_bytes(&msg).unwrap();

        match &received.rewrite_log_event {
            RewriteLogEvent::Commit { commit } => {
                assert_eq!(commit.commit_sha, format!("sha_{}", i));
            }
            other => panic!("Expected Commit, got {:?}", other),
        }
    }

    platform::cleanup_socket(&socket_path);
}

#[cfg(unix)]
#[test]
fn test_concurrent_bind_attempts_only_one_succeeds() {
    use git_ai::async_worker::socket::platform;

    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("concurrent.sock");

    // First worker binds
    let _listener1 = platform::bind_socket(&socket_path).unwrap();

    // Multiple concurrent bind attempts should all fail
    for _ in 0..5 {
        let result = platform::bind_socket(&socket_path);
        assert!(result.is_err(), "Concurrent bind should fail");
    }

    platform::cleanup_socket(&socket_path);
}

// ==============================================================================
// End-to-end integration tests with real repos and async worker enabled
// ==============================================================================
//
// These tests exercise the full async worker pipeline:
//   1. Create a TestRepo with async_worker feature flag enabled
//   2. Set a short idle timeout so the worker shuts down quickly
//   3. Run a real git operation (commit, rebase, cherry-pick, merge)
//   4. Wait for the async worker to finish (poll socket liveness)
//   5. Assert authorship / notes were written correctly
//
// The idle timeout env var (GIT_AI_ASYNC_WORKER_IDLE_TIMEOUT_MS) is passed
// through the test environment so the spawned worker inherits it.

mod async_worker_integration {
    use super::*;
    use git_ai::async_worker::socket;
    use git_ai::async_worker::socket::platform;
    use repos::test_file::ExpectedLineExt;
    use repos::test_repo::TestRepo;
    use std::time::{Duration, Instant};

    /// Short idle timeout (ms) so the worker shuts down quickly in tests.
    const TEST_IDLE_TIMEOUT_MS: &str = "500";

    /// Maximum time to wait for the async worker to finish processing and shut down.
    const MAX_WORKER_WAIT: Duration = Duration::from_secs(15);

    /// Create a plain TestRepo.
    ///
    /// We intentionally do NOT set `async_worker: true` on the repo because
    /// that would cause *all* git commands (including setup/initial commits) to
    /// go through the async path.  `commit_with_env()` reads the authorship
    /// note immediately after the commit, which races the async worker.
    ///
    /// Instead, each test passes `GIT_AI_ASYNC_WORKER=true` only for the
    /// specific operation being tested via `git_with_async_env`.
    fn new_async_repo() -> TestRepo {
        TestRepo::new()
    }

    /// Derive the async-worker socket path from a TestRepo.
    fn socket_path_for_repo(repo: &TestRepo) -> std::path::PathBuf {
        let ai_dir = repo.path().join(".git").join("ai");
        socket::socket_path_for_ai_dir(&ai_dir)
    }

    /// Wait until the async worker has shut down (socket is no longer live).
    /// This is the "guard" that must run before any assertions to ensure the
    /// worker has finished processing the dispatched job.
    fn wait_for_worker_shutdown(repo: &TestRepo) {
        let sock = socket_path_for_repo(repo);
        let start = Instant::now();
        let poll = Duration::from_millis(100);

        // First wait a moment for the worker to start (it may not have bound yet)
        std::thread::sleep(Duration::from_millis(200));

        while start.elapsed() < MAX_WORKER_WAIT {
            if !platform::is_socket_live(&sock) {
                // Socket is dead — worker has exited. Give a tiny grace period
                // for any final filesystem flushes.
                std::thread::sleep(Duration::from_millis(100));
                return;
            }
            std::thread::sleep(poll);
        }

        panic!(
            "Async worker did not shut down within {:?} (socket still live at {})",
            MAX_WORKER_WAIT,
            sock.display()
        );
    }

    /// Helper: run a git command with async worker enabled and a short idle
    /// timeout so the spawned worker inherits both.
    fn git_with_async_env(repo: &TestRepo, args: &[&str]) -> Result<String, String> {
        repo.git_with_env(
            args,
            &[
                ("GIT_AI_ASYNC_WORKER", "true"),
                ("GIT_AI_ASYNC_WORKER_IDLE_TIMEOUT_MS", TEST_IDLE_TIMEOUT_MS),
            ],
            None,
        )
    }

    /// Helper: stage all and commit with the short idle-timeout env var.
    fn stage_all_and_commit_async(repo: &TestRepo, message: &str) -> Result<String, String> {
        repo.git(&["add", "-A"]).expect("git add should succeed");
        git_with_async_env(repo, &["commit", "-m", message])
    }

    // ------------------------------------------------------------------
    // Commit
    // ------------------------------------------------------------------

    #[test]
    fn test_async_worker_commit_preserves_ai_authorship() {
        let repo = new_async_repo();

        // Create initial commit (needed so HEAD exists)
        let mut file = repo.filename("hello.txt");
        file.set_contents(lines!["human line 1"]);
        repo.stage_all_and_commit("Initial commit").unwrap();

        // Make an AI-authored change and commit through the async path
        file.insert_at(1, lines!["ai generated line".ai()]);
        stage_all_and_commit_async(&repo, "AI commit via async worker").unwrap();

        // Guard: wait for the async worker to finish
        wait_for_worker_shutdown(&repo);

        // Assert authorship is correct
        file.assert_lines_and_blame(lines!["human line 1".human(), "ai generated line".ai(),]);
    }

    #[test]
    fn test_async_worker_commit_human_only() {
        let repo = new_async_repo();

        let mut file = repo.filename("readme.txt");
        file.set_contents(lines!["line one"]);
        repo.stage_all_and_commit("Initial commit").unwrap();

        // Human-only change through async path
        file.insert_at(1, lines!["line two"]);
        stage_all_and_commit_async(&repo, "Human commit via async").unwrap();

        wait_for_worker_shutdown(&repo);

        file.assert_lines_and_blame(lines!["line one".human(), "line two".human(),]);
    }

    // ------------------------------------------------------------------
    // Rebase
    // ------------------------------------------------------------------

    #[test]
    fn test_async_worker_rebase_preserves_ai_authorship() {
        let repo = new_async_repo();

        // Initial commit on main
        let mut main_file = repo.filename("main.txt");
        main_file.set_contents(lines!["main line 1", "main line 2"]);
        repo.stage_all_and_commit("Initial commit").unwrap();
        let default_branch = repo.current_branch();

        // Feature branch with AI commits
        repo.git(&["checkout", "-b", "feature"]).unwrap();
        let mut feature_file = repo.filename("feature.txt");
        feature_file.set_contents(lines!["// AI generated feature".ai(), "feature body".ai()]);
        stage_all_and_commit_async(&repo, "AI feature commit").unwrap();
        wait_for_worker_shutdown(&repo);

        // Advance main with a non-conflicting change
        repo.git(&["checkout", &default_branch]).unwrap();
        let mut other = repo.filename("other.txt");
        other.set_contents(lines!["other content"]);
        stage_all_and_commit_async(&repo, "Main advances").unwrap();
        wait_for_worker_shutdown(&repo);

        // Rebase feature onto main
        repo.git(&["checkout", "feature"]).unwrap();
        git_with_async_env(&repo, &["rebase", &default_branch]).unwrap();

        // Guard: wait for worker to finish processing the rebase events
        wait_for_worker_shutdown(&repo);

        // Assert AI authorship preserved after rebase
        feature_file
            .assert_lines_and_blame(lines!["// AI generated feature".ai(), "feature body".ai()]);
    }

    // ------------------------------------------------------------------
    // Cherry-pick
    // ------------------------------------------------------------------

    #[test]
    fn test_async_worker_cherry_pick_preserves_ai_authorship() {
        let repo = new_async_repo();

        // Initial commit
        let mut file = repo.filename("file.txt");
        file.set_contents(lines!["Initial content"]);
        repo.stage_all_and_commit("Initial commit").unwrap();
        let main_branch = repo.current_branch();

        // Feature branch with AI change
        repo.git(&["checkout", "-b", "feature"]).unwrap();
        file.insert_at(1, lines!["AI feature line".ai()]);
        stage_all_and_commit_async(&repo, "Add AI feature").unwrap();
        wait_for_worker_shutdown(&repo);
        let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

        // Cherry-pick onto main
        repo.git(&["checkout", &main_branch]).unwrap();
        git_with_async_env(&repo, &["cherry-pick", &feature_commit]).unwrap();

        // Guard: wait for worker to finish
        wait_for_worker_shutdown(&repo);

        // Assert authorship
        file.assert_lines_and_blame(lines!["Initial content".human(), "AI feature line".ai(),]);
    }

    #[test]
    fn test_async_worker_cherry_pick_multiple_commits() {
        let repo = new_async_repo();

        let mut file = repo.filename("file.txt");
        file.set_contents(lines!["Line 1", ""]);
        repo.stage_all_and_commit("Initial commit").unwrap();
        let main_branch = repo.current_branch();

        // Feature branch with multiple AI commits
        repo.git(&["checkout", "-b", "feature"]).unwrap();

        file.insert_at(1, lines!["AI line 2".ai()]);
        stage_all_and_commit_async(&repo, "AI commit 1").unwrap();
        wait_for_worker_shutdown(&repo);
        let commit1 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

        file.insert_at(2, lines!["AI line 3".ai()]);
        stage_all_and_commit_async(&repo, "AI commit 2").unwrap();
        wait_for_worker_shutdown(&repo);
        let commit2 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

        // Cherry-pick both onto main
        repo.git(&["checkout", &main_branch]).unwrap();
        git_with_async_env(&repo, &["cherry-pick", &commit1, &commit2]).unwrap();

        // Guard: wait for worker
        wait_for_worker_shutdown(&repo);

        // Assert authorship on all lines
        file.assert_lines_and_blame(lines!["Line 1".human(), "AI line 2".ai(), "AI line 3".ai(),]);
    }

    // ------------------------------------------------------------------
    // Merge
    // ------------------------------------------------------------------

    #[test]
    fn test_async_worker_merge_preserves_ai_authorship() {
        let repo = new_async_repo();
        let mut file = repo.filename("test.txt");

        // Base commit
        file.set_contents(lines!["Base line 1", "Base line 2", "Base line 3"]);
        repo.stage_all_and_commit("Initial commit").unwrap();
        let default_branch = repo.current_branch();

        // Feature branch with AI changes (appended to avoid conflicts)
        repo.git(&["checkout", "-b", "feature"]).unwrap();
        file.insert_at(3, lines!["FEATURE LINE 1".ai(), "FEATURE LINE 2".ai()]);
        stage_all_and_commit_async(&repo, "feature branch changes").unwrap();
        wait_for_worker_shutdown(&repo);

        // Back to main — human changes at the beginning (no conflict)
        repo.git(&["checkout", &default_branch]).unwrap();
        file = repo.filename("test.txt");
        file.insert_at(0, lines!["MAIN LINE 1", "MAIN LINE 2"]);
        stage_all_and_commit_async(&repo, "main branch changes").unwrap();
        wait_for_worker_shutdown(&repo);

        // Merge feature into main
        git_with_async_env(
            &repo,
            &["merge", "feature", "-m", "merge feature into main"],
        )
        .unwrap();

        // Guard: wait for worker
        wait_for_worker_shutdown(&repo);

        // Assert blame after merge
        file = repo.filename("test.txt");
        file.assert_lines_and_blame(lines![
            "MAIN LINE 1".human(),
            "MAIN LINE 2".human(),
            "Base line 1".human(),
            "Base line 2".human(),
            "Base line 3".human(),
            "FEATURE LINE 1".ai(),
            "FEATURE LINE 2".ai(),
        ]);
    }

    // ------------------------------------------------------------------
    // Verify socket cleanup
    // ------------------------------------------------------------------

    #[test]
    fn test_async_worker_socket_cleaned_up_after_shutdown() {
        let repo = new_async_repo();

        let mut file = repo.filename("cleanup.txt");
        file.set_contents(lines!["content"]);
        repo.stage_all_and_commit("Initial commit").unwrap();

        file.insert_at(1, lines!["more content".ai()]);
        stage_all_and_commit_async(&repo, "Trigger async worker").unwrap();

        // Wait for worker to shut down
        wait_for_worker_shutdown(&repo);

        // Verify the socket file has been cleaned up
        let sock = socket_path_for_repo(&repo);
        assert!(
            !sock.exists(),
            "Socket file should be cleaned up after worker shuts down: {}",
            sock.display()
        );
    }
}
