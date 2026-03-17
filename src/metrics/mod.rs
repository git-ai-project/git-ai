//! Metrics tracking module.
//!
//! This module provides functionality for recording metric events.
//! Events are written directly to the observability log file.
//!
//! All public types are re-exported for external use (e.g., ingestion server).

pub mod attrs;
pub mod db;
pub mod events;
pub mod pos_encoded;
pub mod types;

// Re-export all public types for external crates
pub use attrs::EventAttributes;
pub use events::{AgentUsageValues, CheckpointValues, CommittedValues, InstallHooksValues};
pub use pos_encoded::PosEncoded;
pub use types::{EventValues, METRICS_API_VERSION, MetricEvent, MetricsBatch};

/// Record an event with values and attributes.
///
/// Events are written immediately to the observability log file.
/// The `flush-logs` command will then upload metrics envelopes to the API
/// or store them in SQLite for later upload.
///
/// # Example
///
/// ```ignore
/// use crate::metrics::{record, CommittedValues, EventAttributes};
///
/// let values = CommittedValues::new()
///     .commit_sha("abc123...")
///     .human_additions(50)
///     .git_diff_added_lines(150)
///     .git_diff_deleted_lines(20)
///     .tool_model_pairs(vec!["all".to_string()])
///     .ai_additions(vec![100]);
///
/// let attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
///     .repo_url("https://github.com/user/repo")
///     .author("user@example.com")
///     .tool("claude-code");
///
/// record(values, attrs);
/// ```
pub fn record<V: EventValues>(values: V, attrs: EventAttributes) {
    let event = MetricEvent::new(&values, attrs.to_sparse());
    // Write directly to observability log
    crate::observability::log_metrics(vec![event]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::MetricEventId;

    #[test]
    fn test_record_creates_event() {
        // This test verifies that record() creates a valid MetricEvent
        // The actual write to the log file happens via observability::log_metrics()
        let values = CommittedValues::new()
            .human_additions(5)
            .git_diff_added_lines(10)
            .git_diff_deleted_lines(5)
            .tool_model_pairs(vec!["all".to_string()])
            .ai_additions(vec![10]);

        let attrs = EventAttributes::with_version("1.0.0")
            .tool("test")
            .commit_sha("test-commit");

        // Create the event manually to verify structure
        let event = MetricEvent::new(&values, attrs.to_sparse());
        assert_eq!(event.event_id, MetricEventId::Committed as u16);
        assert!(event.timestamp > 0);
    }
}
