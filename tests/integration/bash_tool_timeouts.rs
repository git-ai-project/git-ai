//! E2E tests for bash tool hard timeout behaviour.
//!
//! Verifies that:
//! - A snapshot walk that exceeds WALK_TIMEOUT returns `Err` immediately.
//! - A pre-hook walk timeout is swallowed (pre-hook still returns Ok) so the
//!   user's tool call is not blocked.
//! - A post-hook walk timeout returns `BashCheckpointAction::Fallback`.
//! - A hook-level timeout (the 4 s hard limit) returns `Fallback` at every
//!   check-point (after daemon query, after snapshot, before capture).
//!
//! Timeouts are injected via thread-local overrides so parallel tests in other
//! modules are never affected.

use crate::repos::test_repo::TestRepo;
use git_ai::commands::checkpoint_agent::bash_tool::{
    BashCheckpointAction, HookEvent, handle_bash_tool, reset_timeout_overrides_for_test,
    set_hook_timeout_ms_for_test, set_walk_timeout_ms_for_test, snapshot,
};
use std::fs;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn repo_root(repo: &TestRepo) -> std::path::PathBuf {
    repo.canonical_path()
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

/// A walk timeout during pre-hook must NOT propagate as Err to the caller —
/// the hook swallows it so the user's bash tool call is never blocked.
/// The action is still TakePreSnapshot (no snapshot stored).
#[test]
fn test_pre_hook_walk_timeout_swallows_error() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    set_walk_timeout_ms_for_test(0);
    let result = handle_bash_tool(HookEvent::PreToolUse, &root, "wt-sess", "wt-pre-swallow");
    reset_timeout_overrides_for_test();

    let r = result.expect("pre-hook must not return Err even on walk timeout");
    assert!(
        matches!(r.action, BashCheckpointAction::TakePreSnapshot),
        "pre-hook walk timeout should yield TakePreSnapshot (no snapshot stored)"
    );
}

/// A walk timeout during the post-hook must return Fallback, not Err.
#[test]
fn test_post_hook_walk_timeout_returns_fallback() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Successful pre-hook first (no timeout override).
    handle_bash_tool(HookEvent::PreToolUse, &root, "wt-sess", "wt-post-walk")
        .expect("pre-hook should succeed");

    // Write a file so the post-hook has something to snapshot.
    fs::write(root.join("changed.txt"), "new content").expect("file write should succeed");

    set_walk_timeout_ms_for_test(0);
    let result = handle_bash_tool(HookEvent::PostToolUse, &root, "wt-sess", "wt-post-walk");
    reset_timeout_overrides_for_test();

    let r = result.expect("post-hook must not return Err on walk timeout");
    assert!(
        matches!(r.action, BashCheckpointAction::Fallback),
        "post-hook walk timeout should yield Fallback; got action: {:?}",
        match r.action {
            BashCheckpointAction::TakePreSnapshot => "TakePreSnapshot",
            BashCheckpointAction::NoChanges => "NoChanges",
            BashCheckpointAction::Checkpoint(_) => "Checkpoint",
            BashCheckpointAction::Fallback => "Fallback",
        }
    );
}

// ---------------------------------------------------------------------------
// Hook-level timeout tests (the 4 s hard limit)
// ---------------------------------------------------------------------------

/// A hook-level timeout during the pre-hook (fires after the daemon query)
/// must return Fallback rather than proceeding or panicking.
#[test]
fn test_pre_hook_hook_timeout_returns_fallback() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    set_hook_timeout_ms_for_test(0);
    let result = handle_bash_tool(HookEvent::PreToolUse, &root, "ht-sess", "ht-pre");
    reset_timeout_overrides_for_test();

    let r = result.expect("pre-hook must not return Err on hook timeout");
    assert!(
        matches!(r.action, BashCheckpointAction::Fallback),
        "pre-hook hook timeout should yield Fallback"
    );
}

/// A hook-level timeout during the post-hook (fires after load + before
/// snapshot) must return Fallback.
#[test]
fn test_post_hook_hook_timeout_returns_fallback() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Successful pre-hook so a snapshot file exists on disk.
    handle_bash_tool(HookEvent::PreToolUse, &root, "ht-sess", "ht-post")
        .expect("pre-hook should succeed");

    fs::write(root.join("ht_changed.txt"), "content").expect("file write should succeed");

    set_hook_timeout_ms_for_test(0);
    let result = handle_bash_tool(HookEvent::PostToolUse, &root, "ht-sess", "ht-post");
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
    handle_bash_tool(HookEvent::PreToolUse, &root, "reset-sess", "reset-t1")
        .expect("pre-hook should succeed after reset");

    fs::write(root.join("reset_check.txt"), "hello").expect("write should succeed");

    let result = handle_bash_tool(HookEvent::PostToolUse, &root, "reset-sess", "reset-t1")
        .expect("post-hook should succeed after reset");

    assert!(
        matches!(
            result.action,
            BashCheckpointAction::Checkpoint(_) | BashCheckpointAction::NoChanges
        ),
        "normal round-trip after reset should not return Fallback"
    );
}
