/// Comprehensive tests for checkpoint performance target tracking
use git_ai::authorship::working_log::CheckpointKind;
use git_ai::observability::performance_targets::log_performance_for_checkpoint;
use std::time::Duration;

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
fn test_checkpoint_target_exact_boundary() {
    // Test checkpoint at exact 50ms per file boundary
    let files_edited = 10;
    let duration = Duration::from_millis(500); // Exactly 50ms per file

    log_performance_for_checkpoint(files_edited, duration, CheckpointKind::Human);
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
