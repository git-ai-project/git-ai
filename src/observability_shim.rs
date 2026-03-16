/// Shim layer for observability. No-ops when `cloud` feature is disabled.
/// This file exists to minimize cfg gates at call sites for fork maintainability.

#[cfg(feature = "cloud")]
pub use crate::observability::{log_error, log_message, log_performance, spawn_background_flush};

#[cfg(not(feature = "cloud"))]
pub fn log_error(_error: &dyn std::error::Error, _context: Option<serde_json::Value>) {}

#[cfg(not(feature = "cloud"))]
pub fn log_message(_message: &str, _level: &str, _context: Option<serde_json::Value>) {}

#[cfg(not(feature = "cloud"))]
pub fn log_performance(
    _operation: &str,
    _duration: std::time::Duration,
    _context: Option<serde_json::Value>,
    _tags: Option<std::collections::HashMap<String, String>>,
) {
}

#[cfg(not(feature = "cloud"))]
pub fn spawn_background_flush() {}

// --- Performance targets: always available so test infrastructure compiles ---

#[cfg(feature = "cloud")]
pub use crate::observability::wrapper_performance_targets::{
    BenchmarkResult, PERFORMANCE_FLOOR_MS, log_performance_for_checkpoint,
    log_performance_target_if_violated,
};

#[cfg(not(feature = "cloud"))]
pub use self::perf_targets_stub::*;

#[cfg(not(feature = "cloud"))]
mod perf_targets_stub {
    use crate::authorship::working_log::CheckpointKind;
    use std::time::Duration;

    pub const PERFORMANCE_FLOOR_MS: Duration = Duration::from_millis(270);

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct BenchmarkResult {
        pub total_duration: Duration,
        pub git_duration: Duration,
        pub post_command_duration: Duration,
        pub pre_command_duration: Duration,
    }

    pub fn log_performance_target_if_violated(
        _command: &str,
        _pre_command: Duration,
        _git_duration: Duration,
        _post_command: Duration,
    ) {
    }

    pub fn log_performance_for_checkpoint(
        _files_edited: usize,
        _duration: Duration,
        _checkpoint_kind: CheckpointKind,
    ) {
    }
}
