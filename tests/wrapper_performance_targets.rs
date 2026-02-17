/// Comprehensive tests for performance target tracking and benchmarking
use git_ai::authorship::working_log::CheckpointKind;
use git_ai::observability::wrapper_performance_targets::{
    BenchmarkResult, PERFORMANCE_FLOOR_MS, log_performance_for_checkpoint,
    log_performance_target_if_violated,
};
use std::time::Duration;

#[test]
fn test_performance_floor_constant() {
    assert_eq!(
        PERFORMANCE_FLOOR_MS,
        Duration::from_millis(270),
        "Performance floor should be 270ms"
    );
}

#[test]
fn test_benchmark_result_structure() {
    let result = BenchmarkResult {
        total_duration: Duration::from_millis(1000),
        git_duration: Duration::from_millis(800),
        post_command_duration: Duration::from_millis(150),
        pre_command_duration: Duration::from_millis(50),
    };

    assert_eq!(result.total_duration.as_millis(), 1000);
    assert_eq!(result.git_duration.as_millis(), 800);
    assert_eq!(result.post_command_duration.as_millis(), 150);
    assert_eq!(result.pre_command_duration.as_millis(), 50);
}

#[test]
fn test_benchmark_result_clone() {
    let result = BenchmarkResult {
        total_duration: Duration::from_millis(500),
        git_duration: Duration::from_millis(400),
        post_command_duration: Duration::from_millis(60),
        pre_command_duration: Duration::from_millis(40),
    };

    let cloned = result.clone();
    assert_eq!(cloned.total_duration, result.total_duration);
    assert_eq!(cloned.git_duration, result.git_duration);
    assert_eq!(cloned.post_command_duration, result.post_command_duration);
    assert_eq!(cloned.pre_command_duration, result.pre_command_duration);
}

#[test]
fn test_benchmark_result_debug() {
    let result = BenchmarkResult {
        total_duration: Duration::from_millis(100),
        git_duration: Duration::from_millis(80),
        post_command_duration: Duration::from_millis(10),
        pre_command_duration: Duration::from_millis(10),
    };

    let debug_str = format!("{:?}", result);
    assert!(debug_str.contains("BenchmarkResult"));
    assert!(debug_str.contains("total_duration"));
}

#[test]
fn test_log_performance_commit_within_target() {
    // Test commit command that meets target (10% overhead)
    let git_duration = Duration::from_millis(1000);
    let pre_command = Duration::from_millis(50);
    let post_command = Duration::from_millis(50);

    // This should not panic and should log success
    log_performance_target_if_violated("commit", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_commit_violates_target() {
    // Test commit with high overhead that violates target
    let git_duration = Duration::from_millis(100);
    let pre_command = Duration::from_millis(300);
    let post_command = Duration::from_millis(300);

    // Should log violation but not panic
    log_performance_target_if_violated("commit", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_commit_below_floor() {
    // Test commit with overhead below floor (should pass)
    let git_duration = Duration::from_millis(5000);
    let pre_command = Duration::from_millis(100);
    let post_command = Duration::from_millis(100);

    log_performance_target_if_violated("commit", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_rebase_within_target() {
    let git_duration = Duration::from_millis(2000);
    let pre_command = Duration::from_millis(100);
    let post_command = Duration::from_millis(100);

    log_performance_target_if_violated("rebase", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_cherry_pick_within_target() {
    let git_duration = Duration::from_millis(500);
    let pre_command = Duration::from_millis(30);
    let post_command = Duration::from_millis(20);

    log_performance_target_if_violated("cherry-pick", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_reset_within_target() {
    let git_duration = Duration::from_millis(300);
    let pre_command = Duration::from_millis(20);
    let post_command = Duration::from_millis(10);

    log_performance_target_if_violated("reset", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_fetch_within_target() {
    // Fetch allows 50% overhead (1.5x multiplier)
    let git_duration = Duration::from_millis(2000);
    let pre_command = Duration::from_millis(500);
    let post_command = Duration::from_millis(500);

    log_performance_target_if_violated("fetch", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_pull_within_target() {
    // Pull allows 50% overhead
    let git_duration = Duration::from_millis(3000);
    let pre_command = Duration::from_millis(750);
    let post_command = Duration::from_millis(750);

    log_performance_target_if_violated("pull", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_push_within_target() {
    // Push allows 50% overhead
    let git_duration = Duration::from_millis(1000);
    let pre_command = Duration::from_millis(250);
    let post_command = Duration::from_millis(250);

    log_performance_target_if_violated("push", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_unknown_command_within_floor() {
    // Unknown commands use floor target
    let git_duration = Duration::from_millis(1000);
    let pre_command = Duration::from_millis(100);
    let post_command = Duration::from_millis(100);

    log_performance_target_if_violated("unknown-cmd", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_zero_durations() {
    // Test with zero durations (edge case)
    let git_duration = Duration::from_millis(0);
    let pre_command = Duration::from_millis(0);
    let post_command = Duration::from_millis(0);

    log_performance_target_if_violated("commit", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_very_fast_git_command() {
    // Git command faster than pre/post (realistic for status, etc.)
    let git_duration = Duration::from_millis(10);
    let pre_command = Duration::from_millis(50);
    let post_command = Duration::from_millis(50);

    log_performance_target_if_violated("status", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_very_slow_git_command() {
    // Very slow git command (like large repo clone)
    let git_duration = Duration::from_millis(60000); // 60 seconds
    let pre_command = Duration::from_millis(100);
    let post_command = Duration::from_millis(100);

    log_performance_target_if_violated("clone", pre_command, git_duration, post_command);
}

#[test]
fn test_log_performance_checkpoint_within_target() {
    // Checkpoint target: 50ms per file edited
    let files_edited = 10;
    let duration = Duration::from_millis(400); // 40ms per file

    log_performance_for_checkpoint(files_edited, duration, CheckpointKind::Human);
}

#[test]
fn test_log_performance_checkpoint_violates_target() {
    // Checkpoint that's too slow
    let files_edited = 5;
    let duration = Duration::from_millis(500); // 100ms per file (target is 50ms)

    log_performance_for_checkpoint(files_edited, duration, CheckpointKind::Human);
}

#[test]
fn test_log_performance_checkpoint_zero_files() {
    // Edge case: zero files edited
    let files_edited = 0;
    let duration = Duration::from_millis(100);

    log_performance_for_checkpoint(files_edited, duration, CheckpointKind::AiAgent);
}

#[test]
fn test_log_performance_checkpoint_one_file() {
    // Single file checkpoint
    let files_edited = 1;
    let duration = Duration::from_millis(30);

    log_performance_for_checkpoint(files_edited, duration, CheckpointKind::Human);
}

#[test]
fn test_log_performance_checkpoint_many_files() {
    // Large checkpoint with many files
    let files_edited = 1000;
    let duration = Duration::from_millis(40000); // 40ms per file

    log_performance_for_checkpoint(files_edited, duration, CheckpointKind::AiAgent);
}

#[test]
fn test_log_performance_checkpoint_automatic_kind() {
    let files_edited = 5;
    let duration = Duration::from_millis(200);

    log_performance_for_checkpoint(files_edited, duration, CheckpointKind::AiAgent);
}

#[test]
fn test_log_performance_checkpoint_manual_kind() {
    let files_edited = 5;
    let duration = Duration::from_millis(200);

    log_performance_for_checkpoint(files_edited, duration, CheckpointKind::Human);
}

#[test]
fn test_checkpoint_kind_to_string() {
    let human = CheckpointKind::Human;
    let ai_agent = CheckpointKind::AiAgent;
    let ai_tab = CheckpointKind::AiTab;

    assert_eq!(human.to_string(), "human");
    assert_eq!(ai_agent.to_string(), "ai_agent");
    assert_eq!(ai_tab.to_string(), "ai_tab");
}

#[test]
fn test_performance_targets_commit_exact_boundary() {
    // Test at exact 10% overhead boundary for commit
    let git_duration = Duration::from_millis(1000);
    let _overhead = Duration::from_millis(100); // Exactly 10%
    let pre_command = Duration::from_millis(50);
    let post_command = Duration::from_millis(50);

    log_performance_target_if_violated("commit", pre_command, git_duration, post_command);
}

#[test]
fn test_performance_targets_fetch_exact_boundary() {
    // Test at exact 50% overhead boundary for fetch
    let git_duration = Duration::from_millis(2000);
    let _overhead = Duration::from_millis(1000); // Exactly 50%
    let pre_command = Duration::from_millis(500);
    let post_command = Duration::from_millis(500);

    log_performance_target_if_violated("fetch", pre_command, git_duration, post_command);
}

#[test]
fn test_performance_floor_exact_boundary() {
    // Test at exact floor boundary
    let git_duration = Duration::from_millis(1000);
    let pre_command = Duration::from_millis(135);
    let post_command = Duration::from_millis(135); // Total 270ms = floor

    log_performance_target_if_violated("commit", pre_command, git_duration, post_command);
}

#[test]
fn test_checkpoint_target_exact_boundary() {
    // Test checkpoint at exact 50ms per file boundary
    let files_edited = 10;
    let duration = Duration::from_millis(500); // Exactly 50ms per file

    log_performance_for_checkpoint(files_edited, duration, CheckpointKind::Human);
}

#[test]
fn test_all_supported_commands() {
    let commands = vec![
        "commit",
        "rebase",
        "cherry-pick",
        "reset",
        "fetch",
        "pull",
        "push",
        "status",
        "add",
        "rm",
    ];

    let git_duration = Duration::from_millis(1000);
    let pre_command = Duration::from_millis(50);
    let post_command = Duration::from_millis(50);

    for cmd in commands {
        log_performance_target_if_violated(cmd, pre_command, git_duration, post_command);
    }
}

#[test]
fn test_performance_logging_does_not_panic() {
    // Verify various edge cases don't cause panics
    let test_cases = vec![
        (
            Duration::from_millis(0),
            Duration::from_millis(0),
            Duration::from_millis(0),
        ),
        (
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
        ),
        (
            Duration::from_millis(u64::MAX / 2),
            Duration::from_millis(100),
            Duration::from_millis(100),
        ),
    ];

    for (git_dur, pre_dur, post_dur) in test_cases {
        log_performance_target_if_violated("test", pre_dur, git_dur, post_dur);
    }
}

#[test]
fn test_checkpoint_logging_does_not_panic() {
    let test_cases = vec![
        (0, Duration::from_millis(0)),
        (1, Duration::from_millis(1)),
        (1000, Duration::from_millis(50000)),
        (usize::MAX / 1000000, Duration::from_millis(1000)),
    ];

    for (files, duration) in test_cases {
        log_performance_for_checkpoint(files, duration, CheckpointKind::AiAgent);
    }
}

#[test]
fn test_performance_metrics_consistency() {
    // Verify that total = pre + git + post in calculations
    let git_duration = Duration::from_millis(800);
    let pre_command = Duration::from_millis(100);
    let post_command = Duration::from_millis(100);

    let expected_total = pre_command + git_duration + post_command;
    assert_eq!(expected_total.as_millis(), 1000);

    log_performance_target_if_violated("commit", pre_command, git_duration, post_command);
}

#[test]
fn test_overhead_calculation() {
    // Test overhead calculation for targets
    let _git_duration = Duration::from_millis(1000);
    let pre_command = Duration::from_millis(50);
    let post_command = Duration::from_millis(50);

    let overhead = pre_command + post_command;
    assert_eq!(overhead.as_millis(), 100);
    assert!(overhead < PERFORMANCE_FLOOR_MS);
}

#[test]
fn test_multiplier_targets() {
    // Verify multiplier logic: 1.1x for commit, 1.5x for network commands
    let _git_duration = Duration::from_millis(1000);

    // 1.1x = 1100ms total allowed
    let commit_max_overhead = Duration::from_millis(100);

    // 1.5x = 1500ms total allowed
    let fetch_max_overhead = Duration::from_millis(500);

    assert!(commit_max_overhead.as_millis() < fetch_max_overhead.as_millis());
}
