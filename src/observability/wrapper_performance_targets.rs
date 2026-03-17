use std::{collections::HashMap, ops::Add, time::Duration};

use serde_json::json;

use crate::{
    authorship::working_log::CheckpointKind,
    observability::log_performance,
    utils::{debug_performance_log, debug_performance_log_structured},
};

pub const PERFORMANCE_FLOOR_MS: Duration = Duration::from_millis(270);

/// Performance benchmark result containing timing breakdowns
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct BenchmarkResult {
    pub total_duration: Duration,
    pub git_duration: Duration,
    pub post_command_duration: Duration,
    pub pre_command_duration: Duration,
}

pub fn log_performance_target_if_violated(
    command: &str,
    pre_command: Duration,
    git_duration: Duration,
    post_command: Duration,
) {
    let total_duration = pre_command + git_duration + post_command;
    let git_ai_overhead = pre_command + post_command;
    let within_target: bool = match command {
        "commit" => {
            git_duration.mul_f32(1.1) >= total_duration || git_ai_overhead < PERFORMANCE_FLOOR_MS
        }
        "rebase" => {
            git_duration.mul_f32(1.1) >= total_duration || git_ai_overhead < PERFORMANCE_FLOOR_MS
        }
        "cherry-pick" => {
            git_duration.mul_f32(1.1) >= total_duration || git_ai_overhead < PERFORMANCE_FLOOR_MS
        }
        "reset" => {
            git_duration.mul_f32(1.1) >= total_duration || git_ai_overhead < PERFORMANCE_FLOOR_MS
        }
        "fetch" => {
            git_duration.mul_f32(1.5) >= total_duration || git_ai_overhead < PERFORMANCE_FLOOR_MS
        }
        "pull" => {
            git_duration.mul_f32(1.5) >= total_duration || git_ai_overhead < PERFORMANCE_FLOOR_MS
        }
        "push" => {
            git_duration.mul_f32(1.5) >= total_duration || git_ai_overhead < PERFORMANCE_FLOOR_MS
        }
        _ => git_duration.add(PERFORMANCE_FLOOR_MS) >= total_duration,
    };

    let perf_json = json!({
        "command": command,
        "total_duration_ms": total_duration.as_millis(),
        "git_duration_ms": git_duration.as_millis(),
        "pre_command_duration_ms": pre_command.as_millis(),
        "post_command_duration_ms": post_command.as_millis(),
        "within_target": within_target,
    });

    debug_performance_log_structured(perf_json);

    if !within_target {
        debug_performance_log(&format!(
            "ᕽ Performance target violated for command: {}. Total duration: {}ms, Git duration: {}ms. Pre-command: {}ms, Post-command: {}ms.",
            command,
            total_duration.as_millis(),
            git_duration.as_millis(),
            pre_command.as_millis(),
            post_command.as_millis(),
        ));
        log_performance(
            "performance_target_violated",
            total_duration,
            Some(json!({
                "total_duration": total_duration.as_millis(),
                "git_duration": git_duration.as_millis(),
                "pre_command": pre_command.as_millis(),
                "post_command": post_command.as_millis(),
            })),
            Some(HashMap::from([(
                "command".to_string(),
                command.to_string(),
            )])),
        );
    } else {
        debug_performance_log(&format!(
            "✓ Performance target met for command: {}. Total duration: {}ms, Git duration: {}ms",
            command,
            total_duration.as_millis(),
            git_duration.as_millis(),
        ));
    }
}

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
    debug_performance_log_structured(perf_json);

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

        debug_performance_log(&format!(
            "ᕽ Performance target violated for checkpoint: {}. Total duration. Files edited: {}",
            duration.as_millis(),
            files_edited,
        ));
    } else {
        debug_performance_log(&format!(
            "✓ Performance target met for checkpoint: {}. Total duration. Files edited: {}",
            duration.as_millis(),
            files_edited,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_performance_floor_constant() {
        assert_eq!(PERFORMANCE_FLOOR_MS.as_millis(), 270);
    }

    #[test]
    fn test_log_performance_target_commit_within_target() {
        let pre = Duration::from_millis(50);
        let git = Duration::from_millis(1000);
        let post = Duration::from_millis(50);
        // Total overhead = 100ms < PERFORMANCE_FLOOR_MS (270ms), so should be within target
        log_performance_target_if_violated("commit", pre, git, post);
    }

    #[test]
    fn test_log_performance_target_commit_violated() {
        let pre = Duration::from_millis(200);
        let git = Duration::from_millis(100);
        let post = Duration::from_millis(200);
        // Total overhead = 400ms, git*1.1 = 110ms, so violated
        log_performance_target_if_violated("commit", pre, git, post);
    }

    #[test]
    fn test_log_performance_target_rebase_within() {
        let pre = Duration::from_millis(50);
        let git = Duration::from_millis(500);
        let post = Duration::from_millis(50);
        log_performance_target_if_violated("rebase", pre, git, post);
    }

    #[test]
    fn test_log_performance_target_cherry_pick() {
        let pre = Duration::from_millis(100);
        let git = Duration::from_millis(200);
        let post = Duration::from_millis(100);
        log_performance_target_if_violated("cherry-pick", pre, git, post);
    }

    #[test]
    fn test_log_performance_target_reset() {
        let pre = Duration::from_millis(50);
        let git = Duration::from_millis(150);
        let post = Duration::from_millis(50);
        log_performance_target_if_violated("reset", pre, git, post);
    }

    #[test]
    fn test_log_performance_target_fetch() {
        let pre = Duration::from_millis(100);
        let git = Duration::from_millis(2000);
        let post = Duration::from_millis(100);
        // fetch allows 1.5x git duration, so 2000*1.5=3000 vs 2200 total
        log_performance_target_if_violated("fetch", pre, git, post);
    }

    #[test]
    fn test_log_performance_target_pull() {
        let pre = Duration::from_millis(150);
        let git = Duration::from_millis(1000);
        let post = Duration::from_millis(150);
        log_performance_target_if_violated("pull", pre, git, post);
    }

    #[test]
    fn test_log_performance_target_push() {
        let pre = Duration::from_millis(100);
        let git = Duration::from_millis(1500);
        let post = Duration::from_millis(100);
        log_performance_target_if_violated("push", pre, git, post);
    }

    #[test]
    fn test_log_performance_target_generic_command() {
        let pre = Duration::from_millis(100);
        let git = Duration::from_millis(500);
        let post = Duration::from_millis(100);
        // Generic commands use PERFORMANCE_FLOOR_MS (270ms)
        log_performance_target_if_violated("status", pre, git, post);
    }

    #[test]
    fn test_log_performance_target_unknown_command() {
        let pre = Duration::from_millis(50);
        let git = Duration::from_millis(200);
        let post = Duration::from_millis(50);
        log_performance_target_if_violated("unknown-cmd", pre, git, post);
    }

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

    #[test]
    fn test_benchmark_result_fields() {
        let result = BenchmarkResult {
            total_duration: Duration::from_millis(1000),
            git_duration: Duration::from_millis(800),
            post_command_duration: Duration::from_millis(100),
            pre_command_duration: Duration::from_millis(100),
        };
        assert_eq!(result.total_duration.as_millis(), 1000);
        assert_eq!(result.git_duration.as_millis(), 800);
        assert_eq!(result.post_command_duration.as_millis(), 100);
        assert_eq!(result.pre_command_duration.as_millis(), 100);
    }
}
