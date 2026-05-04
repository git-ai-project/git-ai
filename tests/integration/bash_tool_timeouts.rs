//! E2E tests for bash tool hard timeout behaviour.
//!
//! Verifies that:
//! - A snapshot walk that exceeds WALK_TIMEOUT returns `Err` immediately.
//! - A pre-hook walk timeout propagates as `Err` (orchestrator handles gracefully).
//! - A post-hook walk timeout returns `BashCheckpointAction::Fallback`.
//! - A hook-level timeout (the 4 s hard limit) returns `Fallback` on the post-hook.
//!
//! Timeouts are injected via thread-local overrides so parallel tests in other
//! modules are never affected.

use crate::repos::test_repo::TestRepo;
use git_ai::authorship::working_log::AgentId;
use git_ai::commands::checkpoint_agent::bash_tool::{
    BashCheckpointAction, handle_bash_post_tool_use, handle_bash_pre_tool_use_with_context,
    reset_timeout_overrides_for_test, set_daemon_socket_for_test, set_hook_timeout_ms_for_test,
    set_walk_timeout_ms_for_test, snapshot,
};
use std::fs;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn repo_root(repo: &TestRepo) -> std::path::PathBuf {
    set_daemon_socket_for_test(repo.daemon_control_socket_path());
    repo.canonical_path()
}

fn dummy_agent_id() -> AgentId {
    AgentId {
        tool: "test".to_string(),
        id: "test".to_string(),
        model: String::new(),
    }
}

// ---------------------------------------------------------------------------
// Walk-timeout tests
// ---------------------------------------------------------------------------

/// snapshot() must return Err (not a partial snapshot) when the walk exceeds
/// the walk timeout.  Setting the override to 0 ms guarantees an immediate
/// timeout because `elapsed >= Duration::ZERO` is always true.
#[test]
fn test_snapshot_walk_timeout_returns_err() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Add a few files so the walker loop body is entered at least once.
    for i in 0..5 {
        fs::write(
            root.join(format!("wt_file_{}.txt", i)),
            format!("content {}", i),
        )
        .expect("file write should succeed");
    }

    set_walk_timeout_ms_for_test(0);
    let result = snapshot(&root, "wt-sess", "wt-t1", None);
    reset_timeout_overrides_for_test();

    assert!(
        result.is_err(),
        "snapshot should return Err on walk timeout"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("walk") || err_msg.contains("abandoning"),
        "error message should describe the walk abandonment; got: {err_msg}"
    );
}

/// A walk timeout during pre-hook propagates as Err — the orchestrator
/// handles this gracefully by returning Ok(vec![]).
#[test]
fn test_pre_hook_walk_timeout_returns_err() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    set_walk_timeout_ms_for_test(0);
    let result = handle_bash_pre_tool_use_with_context(
        &root,
        "wt-sess",
        "wt-pre-swallow",
        &dummy_agent_id(),
        None,
    );
    reset_timeout_overrides_for_test();

    assert!(
        result.is_err(),
        "pre-hook should return Err on walk timeout"
    );
}

/// A walk timeout during the post-hook must return Fallback, not Err.
#[test]
fn test_post_hook_walk_timeout_returns_fallback() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Successful pre-hook first (no timeout override).
    handle_bash_pre_tool_use_with_context(
        &root,
        "wt-sess",
        "wt-post-walk",
        &dummy_agent_id(),
        None,
    )
    .expect("pre-hook should succeed");

    // Write a file so the post-hook has something to snapshot.
    fs::write(root.join("changed.txt"), "new content").expect("file write should succeed");

    set_walk_timeout_ms_for_test(0);
    let result = handle_bash_post_tool_use(&root, "wt-sess", "wt-post-walk");
    reset_timeout_overrides_for_test();

    let r = result.expect("post-hook must not return Err on walk timeout");
    assert!(
        matches!(r.action, BashCheckpointAction::Fallback),
        "post-hook walk timeout should yield Fallback; got action: {:?}",
        match r.action {
            BashCheckpointAction::NoChanges => "NoChanges",
            BashCheckpointAction::Checkpoint(_) => "Checkpoint",
            BashCheckpointAction::Fallback => "Fallback",
        }
    );
}

// ---------------------------------------------------------------------------
// Hook-level timeout tests (the 4 s hard limit)
// ---------------------------------------------------------------------------

/// A hook-level timeout during the post-hook (fires after load + before
/// snapshot) must return Fallback.
#[test]
fn test_post_hook_hook_timeout_returns_fallback() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Successful pre-hook so a snapshot exists in the daemon.
    handle_bash_pre_tool_use_with_context(&root, "ht-sess", "ht-post", &dummy_agent_id(), None)
        .expect("pre-hook should succeed");

    fs::write(root.join("ht_changed.txt"), "content").expect("file write should succeed");

    set_hook_timeout_ms_for_test(0);
    let result = handle_bash_post_tool_use(&root, "ht-sess", "ht-post");
    reset_timeout_overrides_for_test();

    let r = result.expect("post-hook must not return Err on hook timeout");
    assert!(
        matches!(r.action, BashCheckpointAction::Fallback),
        "post-hook hook timeout should yield Fallback"
    );
}

/// Verify that normal (non-timeout) operation still works correctly after
/// timeout overrides are cleared, ensuring the reset helpers are effective.
#[test]
fn test_timeout_override_reset_restores_normal_operation() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Set extreme overrides then clear them.
    set_walk_timeout_ms_for_test(0);
    set_hook_timeout_ms_for_test(0);
    reset_timeout_overrides_for_test();

    // Now a normal round-trip should detect a changed file.
    handle_bash_pre_tool_use_with_context(&root, "reset-sess", "reset-t1", &dummy_agent_id(), None)
        .expect("pre-hook should succeed after reset");

    fs::write(root.join("reset_check.txt"), "hello").expect("write should succeed");

    let result = handle_bash_post_tool_use(&root, "reset-sess", "reset-t1")
        .expect("post-hook should succeed after reset");

    assert!(
        matches!(
            result.action,
            BashCheckpointAction::Checkpoint(_) | BashCheckpointAction::NoChanges
        ),
        "normal round-trip after reset should not return Fallback"
    );
}
