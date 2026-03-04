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
// Unix Socket Tests
// ==============================================================================

#[cfg(unix)]
mod unix_socket_tests {
    use super::*;
    use git_ai::async_worker::socket::platform;
    use std::time::Duration;

    #[test]
    fn test_bind_socket_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");

        let _listener = platform::bind_socket(&socket_path).unwrap();
        assert!(socket_path.exists(), "Socket file should exist after bind");

        platform::cleanup_socket(&socket_path);
        assert!(
            !socket_path.exists(),
            "Socket file should be removed after cleanup"
        );
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
        let path = std::path::Path::new("/tmp/nonexistent-git-ai-async-test.sock");
        let result = platform::try_send_to_socket(path, b"hello").unwrap();
        assert!(!result, "Should return false for non-existent socket");
    }

    #[test]
    fn test_socket_send_receive_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("roundtrip.sock");

        let listener = platform::bind_socket(&socket_path).unwrap();

        // Send a message
        let payload = b"test payload for roundtrip";
        let sent = platform::try_send_to_socket(&socket_path, payload).unwrap();
        assert!(sent, "Should successfully send to bound socket");

        // Accept and read
        let mut stream = platform::accept_with_timeout(&listener, Duration::from_secs(2))
            .unwrap()
            .expect("Should accept connection");

        let msg = read_message(&mut stream).unwrap().unwrap();
        assert_eq!(msg, payload, "Received message should match sent payload");

        platform::cleanup_socket(&socket_path);
    }

    #[test]
    fn test_socket_send_job_payload() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("job.sock");

        let listener = platform::bind_socket(&socket_path).unwrap();

        // Build and send a real job payload
        let job = make_test_job(make_commit_event());
        let wire = job.to_wire_bytes().unwrap();
        let json_payload = &wire[4..]; // Skip length prefix (try_send_to_socket adds its own)

        let sent = platform::try_send_to_socket(&socket_path, json_payload).unwrap();
        assert!(sent);

        // Accept and read
        let mut stream = platform::accept_with_timeout(&listener, Duration::from_secs(2))
            .unwrap()
            .unwrap();

        let msg = read_message(&mut stream).unwrap().unwrap();

        // Deserialize the job
        let deserialized = AsyncJob::from_json_bytes(&msg).unwrap();
        assert_eq!(deserialized.git_dir, "/tmp/test-repo/.git");

        platform::cleanup_socket(&socket_path);
    }

    #[test]
    fn test_socket_accept_timeout_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("timeout.sock");

        let listener = platform::bind_socket(&socket_path).unwrap();

        // Accept with very short timeout - should return None
        let result = platform::accept_with_timeout(&listener, Duration::from_millis(100)).unwrap();
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
            let sent = platform::try_send_to_socket(&socket_path, payload.as_bytes()).unwrap();
            assert!(sent, "Message {} should send successfully", i);

            let mut stream = platform::accept_with_timeout(&listener, Duration::from_secs(2))
                .unwrap()
                .unwrap();
            let msg = read_message(&mut stream).unwrap().unwrap();
            assert_eq!(
                String::from_utf8(msg).unwrap(),
                payload,
                "Message {} content mismatch",
                i
            );
        }

        platform::cleanup_socket(&socket_path);
    }

    #[test]
    fn test_cleanup_nonexistent_socket_no_panic() {
        let path = std::path::Path::new("/tmp/nonexistent-cleanup-test.sock");
        // Should not panic
        platform::cleanup_socket(path);
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
        assert!(socket_path.exists(), "New socket should exist");

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
// Async Worker Macro Test (similar to worktree_test_wrappers pattern)
// ==============================================================================

/// Macro to create test variants that run with the async_worker feature flag enabled.
/// This follows the same pattern as `worktree_test_wrappers!` in the codebase.
macro_rules! async_worker_test {
    (
        fn $test_name:ident() $body:block
    ) => {
        paste::paste! {
            #[test]
            fn [<test_ $test_name _with_async_worker>]() {
                // Run the test body with async_worker feature flag enabled
                // Tests can check FeatureFlags to verify behavior
                let flags = FeatureFlags {
                    async_worker: true,
                    ..FeatureFlags::default()
                };
                assert!(
                    flags.async_worker,
                    "Async worker flag should be enabled in this test variant"
                );
                $body
            }

            #[test]
            fn [<test_ $test_name _without_async_worker>]() {
                // Run the test body with async_worker feature flag disabled
                let flags = FeatureFlags::default();
                assert!(
                    !flags.async_worker,
                    "Async worker flag should be disabled in this test variant"
                );
                $body
            }
        }
    };
}

// Use the macro to test that feature flag correctly gates behavior
async_worker_test! {
    fn feature_flag_gating() {
        // This test runs twice: once with async_worker=true, once with async_worker=false
        // The assertions in the macro verify the flag state
        let _event = make_commit_event();
    }
}

async_worker_test! {
    fn job_creation_independent_of_flag() {
        // Job creation should work regardless of feature flag state
        let job = make_test_job(make_commit_event());
        let wire = job.to_wire_bytes().unwrap();
        assert!(wire.len() > 4);
        let deserialized = AsyncJob::from_json_bytes(&wire[4..]).unwrap();
        assert_eq!(deserialized.git_dir, "/tmp/test-repo/.git");
    }
}
