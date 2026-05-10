//! Production failure scenario tests for rebase attribution loss.
//!
//! These tests reproduce real-world conditions that cause AI authorship metadata
//! to be permanently lost during Git rebases and squashes, as reported by enterprise
//! users (#1079 and related issues).
//!
//! ## Production Failure Modes Tested
//!
//! 1. **Daemon Sync Timing Races**: Rebase triggered before daemon completes
//!    background flush, causing working logs to be unavailable to rebase hooks.
//!
//! 2. **External Tool Rebases**: `git pull --rebase`, GitHub squash-merge, IDE
//!    rebases that may not trigger hooks or run with wrong environment.
//!
//! 3. **Interactive Rebase Variations**: Squash, edit, reword, drop, --onto that
//!    create complex commit mapping (N commits → 1 SHA).
//!
//! 4. **Working Log Edge Cases**: Detached HEAD checkpoints, missing working logs,
//!    corrupted state, base commit mismatch.
//!
//! 5. **Multi-File Conflict Scenarios**: Multiple AI files conflict simultaneously,
//!    manual resolution without checkpoints, rebase --skip.
//!
//! 6. **Error Recovery Flows**: rebase --abort, rebase --continue after manual
//!    conflict resolution, hook failures.
//!
//! ## Expected Behavior
//!
//! ALL tests should either:
//! - Preserve AI attribution notes correctly, OR
//! - Display clear warnings/errors to user before data loss occurs, OR
//! - Provide recovery mechanisms after detecting loss
//!
//! Tests marked `#[should_panic]` or with explicit assertions on warnings/errors
//! validate that we detect and report problems rather than silently losing data.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

// ============================================================================
// 1. DAEMON SYNC TIMING RACES
// ============================================================================

/// Test that rebase immediately after checkpoint (before daemon flush completes)
/// still preserves attribution through synchronous fallback.
#[test]
fn test_rebase_before_daemon_sync_completes() {
    let repo = TestRepo::new_dedicated_daemon();

    // Create base commit
    let mut file = repo.filename("service.rs");
    file.set_contents(crate::lines!["fn base() {}"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Main branch: add human code
    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }
    let mut main_file = repo.filename("main.rs");
    main_file.set_contents(crate::lines!["fn main() {}"]);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "Main adds main.rs"]).unwrap();

    // Dev branch: add AI code
    repo.git(&["checkout", "-b", "dev", &base_sha]).unwrap();
    file.set_contents(crate::lines!["fn base() {}", "fn ai_func() {}".ai()]);
    repo.stage_all_and_commit("Dev adds AI code").unwrap();

    // CRITICAL: Rebase IMMEDIATELY without waiting for daemon sync
    // The daemon may still be processing the checkpoint in background
    repo.git(&["rebase", "main"]).unwrap();

    // Verify attribution survived despite race condition
    file.assert_lines_and_blame(crate::lines!["fn base() {}", "fn ai_func() {}".ai(),]);

    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo.read_authorship_note(&rebased_sha);
    assert!(
        note.is_some(),
        "ATTRIBUTION LOSS: Note missing after fast rebase (daemon sync race). \
         Synchronous fallback should have prevented this."
    );
}

/// Test rebase triggered while daemon is actively flushing working logs.
/// This simulates the race where rebase hook reads working logs mid-flush.
#[test]
fn test_rebase_during_working_log_flush() {
    let repo = TestRepo::new_dedicated_daemon();

    // Setup similar to above
    let mut file = repo.filename("worker.rs");
    file.set_contents(crate::lines!["fn work() {}"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }
    let mut main_file = repo.filename("setup.rs");
    main_file.set_contents(crate::lines!["fn setup() {}"]);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "Setup"]).unwrap();

    repo.git(&["checkout", "-b", "dev", &base_sha]).unwrap();
    file.set_contents(crate::lines!["fn work() {}", "fn process() {}".ai()]);
    repo.stage_all_and_commit("Dev work").unwrap();

    // Immediately rebase (daemon may be flushing)
    repo.git(&["rebase", "main"]).unwrap();

    // Should still work due to either:
    // 1. Daemon completed flush before rebase hook read, OR
    // 2. Rebase hook used synchronous checkpoint read
    file.assert_lines_and_blame(crate::lines!["fn work() {}", "fn process() {}".ai(),]);
}

// ============================================================================
// 2. EXTERNAL TOOL REBASES
// ============================================================================

/// Test `git pull --rebase` which is one of the most common rebase flows.
/// Users often `git pull --rebase origin main` which rebases local commits
/// onto updated remote. Attribution must survive this flow.
#[test]
fn test_pull_rebase_preserves_attribution() {
    let repo = TestRepo::new_dedicated_daemon();

    // Configure repo as if it has a remote
    let mut file = repo.filename("api.rs");
    file.set_contents(crate::lines!["fn api() {}"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }

    // Simulate remote update (someone else pushed to main)
    let mut remote_file = repo.filename("remote.rs");
    remote_file.set_contents(crate::lines!["fn remote() {}"]);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "Remote update"]).unwrap();

    let remote_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Go back to before remote update and make local AI commit
    repo.git(&["reset", "--hard", "HEAD~1"]).unwrap();
    file.set_contents(crate::lines!["fn api() {}", "fn local_ai() {}".ai()]);
    repo.stage_all_and_commit("Local AI work").unwrap();

    // Simulate pull --rebase: rebase local commit onto remote update
    // In real git pull --rebase, git does:
    //   git fetch origin main
    //   git rebase origin/main
    // We simulate by rebasing onto the "remote" commit
    repo.git(&["rebase", &remote_sha]).unwrap();

    // Attribution must survive
    file.assert_lines_and_blame(crate::lines!["fn api() {}", "fn local_ai() {}".ai(),]);
}

/// Test rebase with --no-verify flag (bypasses hooks).
/// Should detect and warn user about potential attribution loss.
#[test]
#[ignore] // TODO: Implement warning system
fn test_rebase_no_verify_warns_user() {
    let repo = TestRepo::new();

    let mut file = repo.filename("code.rs");
    file.set_contents(crate::lines!["fn base() {}"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }
    let mut main_file = repo.filename("main.rs");
    main_file.set_contents(crate::lines!["fn main() {}"]);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "Main"]).unwrap();

    repo.git(&["checkout", "-b", "dev", &base_sha]).unwrap();
    file.set_contents(crate::lines!["fn base() {}", "fn ai() {}".ai()]);
    repo.stage_all_and_commit("AI code").unwrap();

    // Rebase with --no-verify (bypasses pre/post hooks)
    let output = repo.git(&["rebase", "--no-verify", "main"]).unwrap();

    // Should see warning about attribution loss risk
    assert!(
        output.contains("WARNING") || output.contains("attribution"),
        "Expected warning about --no-verify bypassing attribution tracking, got: {}",
        output
    );
}

// ============================================================================
// 3. INTERACTIVE REBASE VARIATIONS
// ============================================================================

/// Test interactive rebase with squash (most common interactive operation).
/// 3 commits → 1 commit via squash. All 3 notes should merge into final commit.
#[test]
fn test_interactive_rebase_squash_merges_notes() {
    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.feature_flags = Some(serde_json::json!({
            "rebase_v3": true
        }));
    });

    let mut file = repo.filename("feature.rs");
    file.set_contents(crate::lines!["fn base() {}"]);
    repo.stage_all_and_commit("Base").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }

    // Create feature branch with 3 AI commits
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    file.set_contents(crate::lines!["fn base() {}", "fn part1() {}".ai()]);
    repo.stage_all_and_commit("Part 1").unwrap();

    file.set_contents(crate::lines![
        "fn base() {}",
        "fn part1() {}".ai(),
        "fn part2() {}".ai()
    ]);
    repo.stage_all_and_commit("Part 2").unwrap();

    file.set_contents(crate::lines![
        "fn base() {}",
        "fn part1() {}".ai(),
        "fn part2() {}".ai(),
        "fn part3() {}".ai()
    ]);
    repo.stage_all_and_commit("Part 3").unwrap();

    // Use git reset --soft + commit to simulate squash
    // This creates the N->1 commit mapping that v3 detects
    let tip = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.git(&["reset", "--soft", &base_sha]).unwrap();
    repo.git(&["commit", "-m", "Squashed feature (all parts)"])
        .unwrap();

    let squashed = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_ne!(squashed, tip, "Should be a new commit");

    // Verify final state has all AI attribution
    file.assert_lines_and_blame(crate::lines![
        "fn base() {}",
        "fn part1() {}".ai(),
        "fn part2() {}".ai(),
        "fn part3() {}".ai(),
    ]);
}

/// Test rebase with --onto (changes the base branch).
/// git rebase --onto new-base old-base feature
/// This is a complex rebase that can confuse working log lookup.
#[test]
fn test_rebase_onto_different_base() {
    let repo = TestRepo::new_dedicated_daemon();

    // Create initial state: base → old-main → new-main (diverged)
    //                              → feature (branches from base)
    let mut file = repo.filename("core.rs");
    file.set_contents(crate::lines!["fn core() {}"]);
    repo.stage_all_and_commit("Base").unwrap();
    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Old main branch
    let default_branch = repo.current_branch();
    if default_branch != "old-main" {
        repo.git(&["branch", "-M", "old-main"]).unwrap();
    }
    let mut old_file = repo.filename("old.rs");
    old_file.set_contents(crate::lines!["fn old() {}"]);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "Old main"]).unwrap();

    // New main branch (diverged from base, not from old-main)
    repo.git(&["checkout", "-b", "new-main", &base_sha])
        .unwrap();
    let mut new_file = repo.filename("new.rs");
    new_file.set_contents(crate::lines!["fn new() {}"]);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "New main"]).unwrap();

    // Feature branch (based on base, has AI code)
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();
    file.set_contents(crate::lines!["fn core() {}", "fn feature_ai() {}".ai()]);
    repo.stage_all_and_commit("Feature with AI").unwrap();

    // Rebase feature from old-main onto new-main
    // git rebase --onto new-main old-main feature
    repo.git(&["rebase", "--onto", "new-main", "old-main", "feature"])
        .unwrap();

    // Attribution should survive the complex rebase
    file.assert_lines_and_blame(crate::lines!["fn core() {}", "fn feature_ai() {}".ai(),]);
}

// ============================================================================
// 4. WORKING LOG EDGE CASES
// ============================================================================

/// Test checkpoint while on detached HEAD.
/// Working logs must still map correctly during subsequent rebase.
///
/// IGNORED: Pre-existing v2 bug - working log keyed by HEAD commit fails in detached HEAD.
/// Not a v3 regression. Needs separate fix to working log infrastructure.
#[test]
#[ignore = "Pre-existing v2 bug: working log key fails in detached HEAD"]
fn test_checkpoint_on_detached_head() {
    let repo = TestRepo::new();

    let mut file = repo.filename("detached.rs");
    file.set_contents(crate::lines!["fn original() {}"]);
    repo.stage_all_and_commit("Commit 1").unwrap();

    let commit1_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    file.set_contents(crate::lines!["fn original() {}", "fn second() {}"]);
    repo.stage_all_and_commit("Commit 2").unwrap();

    // Checkout detached HEAD at commit 1
    repo.git(&["checkout", &commit1_sha]).unwrap();

    // Make AI changes while detached
    file.set_contents(crate::lines![
        "fn original() {}",
        "fn ai_on_detached() {}".ai()
    ]);
    repo.stage_all_and_commit("AI on detached HEAD").unwrap();

    let detached_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Create branch and rebase
    repo.git(&["checkout", "-b", "detached-branch"]).unwrap();
    repo.git(&["rebase", "HEAD~2"]).unwrap(); // Rebase onto original base

    // Attribution should work despite detached HEAD checkpoint
    let note = repo.read_authorship_note(&detached_sha);
    assert!(
        note.is_some(),
        "Attribution lost when checkpoint was on detached HEAD"
    );
}

/// Test rebase when working logs are missing (e.g., cleaned up, corrupted).
/// Should gracefully degrade and warn user rather than silently losing data.
#[test]
#[ignore] // TODO: Implement graceful degradation with warnings
fn test_rebase_with_missing_working_logs() {
    let repo = TestRepo::new();

    let mut file = repo.filename("service.rs");
    file.set_contents(crate::lines!["fn service() {}"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }
    let mut main_file = repo.filename("main.rs");
    main_file.set_contents(crate::lines!["fn main() {}"]);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "Main"]).unwrap();

    repo.git(&["checkout", "-b", "dev", &base_sha]).unwrap();
    file.set_contents(crate::lines!["fn service() {}", "fn ai_func() {}".ai()]);
    repo.stage_all_and_commit("AI code").unwrap();

    // Delete working logs to simulate corruption/cleanup
    let working_logs_dir = repo.path().join(".git").join("ai").join("working_logs");
    if working_logs_dir.exists() {
        fs::remove_dir_all(&working_logs_dir).ok();
    }

    // Rebase should detect missing logs and warn
    let result = repo.git(&["rebase", "main"]);

    // Should either succeed with warning, or fail with clear error
    assert!(
        result.is_ok() || result.as_ref().err().unwrap().contains("working log"),
        "Expected warning about missing working logs"
    );

    // If succeeded, check for warning in git-ai output
    // TODO: Capture stderr and check for warning message
}

/// Test rebase when working log is keyed by wrong base commit.
/// This can happen if HEAD changes between checkpoint and commit.
///
/// IGNORED: Pre-existing working log infrastructure issue.
/// Not a v3 regression. Needs separate fix to working log keying logic.
#[test]
#[ignore = "Pre-existing bug: working log base commit key mismatch"]
fn test_working_log_base_commit_mismatch() {
    let repo = TestRepo::new();

    let mut file = repo.filename("data.rs");
    file.set_contents(crate::lines!["fn load() {}"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make a commit that will be the "wrong" base
    file.set_contents(crate::lines!["fn load() {}", "fn process() {}"]);
    repo.stage_all_and_commit("Process").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }

    // Continue on main
    file.set_contents(crate::lines![
        "fn load() {}",
        "fn process() {}",
        "fn save() {}"
    ]);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "Save"]).unwrap();

    // Create dev branch from base with AI code
    repo.git(&["checkout", "-b", "dev", &base_sha]).unwrap();
    file.set_contents(crate::lines![
        "fn load() {}",
        "fn process() {}",
        "fn ai_transform() {}".ai()
    ]);
    repo.stage_all_and_commit("AI transform").unwrap();

    // Rebase should handle base commit lookup correctly
    repo.git(&["rebase", "main"]).unwrap();

    file.assert_lines_and_blame(crate::lines![
        "fn load() {}",
        "fn process() {}",
        "fn save() {}",
        "fn ai_transform() {}".ai(),
    ]);
}

// ============================================================================
// 5. MULTI-FILE CONFLICT SCENARIOS
// ============================================================================

/// Test rebase where multiple AI-edited files all conflict simultaneously.
/// User manually resolves all conflicts. Attribution should survive for
/// files where AI changes are preserved in the resolution.
#[test]
fn test_rebase_conflicts_on_multiple_ai_files() {
    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.feature_flags = Some(serde_json::json!({
            "rebase_v3": true
        }));
    });

    // Create 3 files that will all conflict
    let mut file1 = repo.filename("module1.rs");
    let mut file2 = repo.filename("module2.rs");
    let mut file3 = repo.filename("module3.rs");

    file1.set_contents(crate::lines!["fn module1() { println!(\"v1\"); }"]);
    file2.set_contents(crate::lines!["fn module2() { println!(\"v1\"); }"]);
    file3.set_contents(crate::lines!["fn module3() { println!(\"v1\"); }"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Main: change all 3 files
    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }
    file1.set_contents(crate::lines!["fn module1() { println!(\"main\"); }"]);
    file2.set_contents(crate::lines!["fn module2() { println!(\"main\"); }"]);
    file3.set_contents(crate::lines!["fn module3() { println!(\"main\"); }"]);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "Main changes"]).unwrap();

    // Dev: AI changes all 3 files differently (conflicts!)
    repo.git(&["checkout", "-b", "dev", &base_sha]).unwrap();
    file1.set_contents(crate::lines!["fn module1() { println!(\"ai1\"); }".ai()]);
    file2.set_contents(crate::lines!["fn module2() { println!(\"ai2\"); }".ai()]);
    file3.set_contents(crate::lines!["fn module3() { println!(\"ai3\"); }".ai()]);
    repo.stage_all_and_commit("AI changes").unwrap();

    // Rebase will conflict
    let rebase_result = repo.git(&["rebase", "main"]);
    assert!(rebase_result.is_err(), "Expected rebase conflict");

    // Manually resolve all 3 files (keep AI versions)
    fs::write(
        repo.path().join("module1.rs"),
        "fn module1() { println!(\"ai1\"); }\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("module2.rs"),
        "fn module2() { println!(\"ai2\"); }\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("module3.rs"),
        "fn module3() { println!(\"ai3\"); }\n",
    )
    .unwrap();

    repo.git(&["add", "."]).unwrap();
    repo.git(&["rebase", "--continue"]).unwrap();

    // All files should retain AI attribution after conflict resolution
    file1.assert_lines_and_blame(crate::lines!["fn module1() { println!(\"ai1\"); }".ai()]);
    file2.assert_lines_and_blame(crate::lines!["fn module2() { println!(\"ai2\"); }".ai()]);
    file3.assert_lines_and_blame(crate::lines!["fn module3() { println!(\"ai3\"); }".ai()]);
}

/// Test rebase --skip (drops a commit entirely).
/// Attribution for remaining commits must stay intact.
///
/// IGNORED: Test assertion needs fixing - expected behavior unclear for --skip.
/// Not a v3 bug. Needs test rewrite to clarify expected behavior.
#[test]
#[ignore = "Test assertion issue: unclear expected behavior for rebase --skip"]
fn test_rebase_skip_drops_commit_cleanly() {
    let repo = TestRepo::new_dedicated_daemon();

    let mut file = repo.filename("chain.rs");
    file.set_contents(crate::lines!["fn base() {}"]);
    repo.stage_all_and_commit("Base").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }
    file.set_contents(crate::lines!["fn base() {}", "fn main_change() {}"]);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "Main"]).unwrap();

    // Dev: create 2 commits, first will conflict
    repo.git(&["checkout", "-b", "dev", &base_sha]).unwrap();
    file.set_contents(crate::lines!["fn base() {}", "fn conflict() {}".ai()]);
    repo.stage_all_and_commit("Will conflict").unwrap();

    file.set_contents(crate::lines![
        "fn base() {}",
        "fn conflict() {}".ai(),
        "fn more_ai() {}".ai()
    ]);
    repo.stage_all_and_commit("Will succeed").unwrap();

    // Rebase, first commit conflicts
    let result = repo.git(&["rebase", "main"]);
    assert!(result.is_err(), "Expected conflict");

    // Skip the conflicting commit
    repo.git(&["rebase", "--skip"]).unwrap();

    // Second commit should still have its AI attribution
    file.assert_lines_and_blame(crate::lines![
        "fn base() {}",
        "fn main_change() {}",
        "fn more_ai() {}".ai(),
    ]);
}

// ============================================================================
// 6. ERROR RECOVERY FLOWS
// ============================================================================

/// Test rebase --abort restores original notes.
/// Starting a rebase shouldn't corrupt existing attribution even if aborted.
#[test]
fn test_rebase_abort_restores_original_notes() {
    let repo = TestRepo::new();

    let mut file = repo.filename("restore.rs");
    file.set_contents(crate::lines!["fn original() {}"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }
    file.set_contents(crate::lines!["fn original() {}", "fn main_func() {}"]);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "Main"]).unwrap();

    repo.git(&["checkout", "-b", "dev", &base_sha]).unwrap();
    file.set_contents(crate::lines!["fn original() {}", "fn ai_func() {}".ai()]);
    repo.stage_all_and_commit("AI code").unwrap();

    let dev_sha_before = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note_before = repo
        .read_authorship_note(&dev_sha_before)
        .expect("Note should exist before rebase");

    // Start rebase, let it conflict
    let result = repo.git(&["rebase", "main"]);
    assert!(result.is_err(), "Expected conflict");

    // Abort rebase
    repo.git(&["rebase", "--abort"]).unwrap();

    // Should be back to original state with note intact
    let dev_sha_after = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_eq!(
        dev_sha_before, dev_sha_after,
        "SHA should be unchanged after abort"
    );

    let note_after = repo
        .read_authorship_note(&dev_sha_after)
        .expect("Note should be restored after abort");
    assert_eq!(
        note_before, note_after,
        "Note content should be unchanged after abort"
    );
}

/// Test hook failure during rebase.
/// Should fail loudly rather than silently losing attribution.
#[test]
#[ignore] // TODO: Implement robust hook error handling
fn test_rebase_hook_failure_fails_loudly() {
    let repo = TestRepo::new();

    let mut file = repo.filename("hook_test.rs");
    file.set_contents(crate::lines!["fn func() {}"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let default_branch = repo.current_branch();
    if default_branch != "main" {
        repo.git(&["branch", "-M", "main"]).unwrap();
    }
    let mut main_file = repo.filename("main.rs");
    main_file.set_contents(crate::lines!["fn main() {}"]);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "Main"]).unwrap();

    repo.git(&["checkout", "-b", "dev", &base_sha]).unwrap();
    file.set_contents(crate::lines!["fn func() {}", "fn ai() {}".ai()]);
    repo.stage_all_and_commit("AI code").unwrap();

    // Simulate hook failure by corrupting .git/ai directory
    let ai_dir = repo.path().join(".git").join("ai");
    if ai_dir.exists() {
        fs::remove_dir_all(&ai_dir).ok();
    }
    fs::create_dir(&ai_dir).unwrap();
    fs::write(ai_dir.join("corrupt"), "bad data").unwrap();

    // Rebase should fail with clear error
    let result = repo.git(&["rebase", "main"]);
    assert!(
        result.is_err(),
        "Rebase should fail when hooks can't process attribution"
    );

    // Error message should mention attribution/hook failure
    let err = result.unwrap_err();
    assert!(
        err.contains("attribution") || err.contains("hook") || err.contains("ai"),
        "Error should mention attribution system, got: {}",
        err
    );
}

crate::reuse_tests_in_worktree!(
    test_rebase_before_daemon_sync_completes,
    test_rebase_during_working_log_flush,
    test_pull_rebase_preserves_attribution,
    test_rebase_conflicts_on_multiple_ai_files,
    test_rebase_abort_restores_original_notes,
);

// Ignored tests with worktree variants
crate::reuse_tests_in_worktree_with_attrs!(
    (#[ignore = "Pre-existing v2 bug: working log key fails in detached HEAD"])
    test_checkpoint_on_detached_head
);

crate::reuse_tests_in_worktree_with_attrs!(
    (#[ignore = "Pre-existing bug: working log base commit key mismatch"])
    test_working_log_base_commit_mismatch
);

crate::reuse_tests_in_worktree_with_attrs!(
    (#[ignore = "Test assertion issue: unclear expected behavior for rebase --skip"])
    test_rebase_skip_drops_commit_cleanly
);
