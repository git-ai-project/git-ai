use std::collections::HashMap;
use std::time::Duration;

use serde_json::json;

use crate::{authorship::working_log::CheckpointKind, observability::log_performance};

pub fn log_performance_for_checkpoint(
    files_edited: usize,
    duration: Duration,
    checkpoint_kind: CheckpointKind,
) {
    let within_target = Duration::from_millis(50 * files_edited as u64) >= duration;

    // Output structured JSON for benchmarking (when GIT_AI_DEBUG_PERFORMANCE >= 2)
    // For git-ai commands like checkpoint, there's no pre/post/git breakdown - just total time
    let perf_json = json!({
        "command": "checkpoint",
        "total_duration_ms": duration.as_millis(),
        "git_duration_ms": 0,
        "pre_command_duration_ms": 0,
        "post_command_duration_ms": 0,
        "files_edited": files_edited,
        "checkpoint_kind": checkpoint_kind.to_string(),
        "within_target": within_target,
    });
    tracing::debug!(%perf_json, "performance");

    if !within_target {
        log_performance(
            "checkpoint",
            duration,
            Some(json!({
                "files_edited": files_edited,
                "checkpoint_kind": checkpoint_kind.to_string(),
                "duration": duration.as_millis(),
            })),
            Some(HashMap::from([(
                "checkpoint_kind".to_string(),
                checkpoint_kind.to_string(),
            )])),
        );

        tracing::debug!(
            "Performance target violated for checkpoint: {}. Total duration. Files edited: {}",
            duration.as_millis(),
            files_edited,
        );
    } else {
        tracing::debug!(
            "Performance target met for checkpoint: {}. Total duration. Files edited: {}",
            duration.as_millis(),
            files_edited,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_performance_checkpoint_within_target() {
        // Target: 50ms per file, so 5 files = 250ms target
        log_performance_for_checkpoint(5, Duration::from_millis(200), CheckpointKind::AiAgent);
    }

    #[test]
    fn test_log_performance_checkpoint_violated() {
        // Target: 50ms per file, so 2 files = 100ms target
        log_performance_for_checkpoint(2, Duration::from_millis(150), CheckpointKind::AiTab);
    }

    #[test]
    fn test_log_performance_checkpoint_zero_files() {
        // Zero files means 0ms target, any duration violates
        log_performance_for_checkpoint(0, Duration::from_millis(10), CheckpointKind::Human);
    }

    #[test]
    fn test_log_performance_checkpoint_many_files() {
        // 100 files = 5000ms target
        log_performance_for_checkpoint(100, Duration::from_millis(4000), CheckpointKind::AiAgent);
    }
}
