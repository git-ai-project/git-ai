use std::collections::HashMap;
use std::time::Duration;

use crate::metrics::MetricEvent;

pub mod wrapper_performance_targets;

/// Maximum events per metrics envelope
pub const MAX_METRICS_PER_ENVELOPE: usize = 250;

/// Submit telemetry envelopes via the best available path:
/// 1. External daemon control socket (wrapper processes)
/// 2. In-process daemon telemetry worker (daemon process itself)
/// 3. Silently drop if neither is available
fn submit_telemetry_envelope(envelopes: Vec<crate::daemon::TelemetryEnvelope>) {
    if crate::daemon::telemetry_handle::daemon_telemetry_available() {
        crate::daemon::telemetry_handle::submit_telemetry(envelopes);
    } else if crate::daemon::daemon_process_active() {
        crate::daemon::telemetry_worker::submit_daemon_internal_telemetry(envelopes);
    }
}

/// Log an error to Sentry (via daemon telemetry worker)
pub fn log_error(error: &dyn std::error::Error, context: Option<serde_json::Value>) {
    let envelope = crate::daemon::TelemetryEnvelope::Error {
        timestamp: chrono::Utc::now().to_rfc3339(),
        message: error.to_string(),
        context,
    };
    submit_telemetry_envelope(vec![envelope]);
}

/// Log a performance metric to Sentry (via daemon telemetry worker)
pub fn log_performance(
    operation: &str,
    duration: Duration,
    context: Option<serde_json::Value>,
    tags: Option<HashMap<String, String>>,
) {
    let envelope = crate::daemon::TelemetryEnvelope::Performance {
        timestamp: chrono::Utc::now().to_rfc3339(),
        operation: operation.to_string(),
        duration_ms: duration.as_millis(),
        context,
        tags,
    };
    submit_telemetry_envelope(vec![envelope]);
}

/// Log a message to Sentry (info, warning, etc.) (via daemon telemetry worker)
#[allow(dead_code)]
pub fn log_message(message: &str, level: &str, context: Option<serde_json::Value>) {
    let envelope = crate::daemon::TelemetryEnvelope::Message {
        timestamp: chrono::Utc::now().to_rfc3339(),
        message: message.to_string(),
        level: level.to_string(),
        context,
    };
    submit_telemetry_envelope(vec![envelope]);
}

/// Log a batch of metric events (via daemon telemetry worker).
///
/// Events are batched into envelopes of up to 250 events each.
/// Events originating from the `mock_ai` test preset are silently
/// dropped so they never reach telemetry.
pub fn log_metrics(
    #[cfg_attr(any(test, feature = "test-support"), allow(unused))] events: Vec<MetricEvent>,
) {
    #[cfg(any(test, feature = "test-support"))]
    return;

    #[cfg(not(any(test, feature = "test-support")))]
    {
        let events: Vec<MetricEvent> = events.into_iter().filter(|e| !is_mock_ai(e)).collect();
        if events.is_empty() {
            return;
        }

        // Split into chunks of MAX_METRICS_PER_ENVELOPE
        for chunk in events.chunks(MAX_METRICS_PER_ENVELOPE) {
            let envelope = crate::daemon::TelemetryEnvelope::Metrics {
                events: chunk.to_vec(),
            };
            submit_telemetry_envelope(vec![envelope]);
        }
    }
}

/// Returns `true` when the event originates from the `mock_ai` test preset.
///
/// Checks both the tool attribute (position 20, set for AgentUsage /
/// Checkpoint / InstallHooks events) and the `tool_model_pairs` committed
/// value (position 3, keys like `"mock_ai::unknown"`).
#[cfg_attr(any(test, feature = "test-support"), allow(dead_code))]
fn is_mock_ai(event: &MetricEvent) -> bool {
    use crate::metrics::{MOCK_AI_TOOL, attrs::attr_pos, events::committed_pos};
    use serde_json::Value;

    let tool_pos = attr_pos::TOOL.to_string();
    if let Some(Value::String(tool)) = event.attrs.get(&tool_pos)
        && tool == MOCK_AI_TOOL
    {
        return true;
    }

    let pairs_pos = committed_pos::TOOL_MODEL_PAIRS.to_string();
    if let Some(Value::Array(pairs)) = event.values.get(&pairs_pos)
        && pairs
            .iter()
            .any(|p| matches!(p, Value::String(s) if s.starts_with(MOCK_AI_TOOL)))
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    // Test error logging
    #[test]
    fn test_log_error_no_panic() {
        use std::io;
        let error = io::Error::new(io::ErrorKind::NotFound, "test error");
        log_error(&error, None);
    }

    #[test]
    fn test_log_error_with_context() {
        use serde_json::json;
        use std::io;
        let error = io::Error::new(io::ErrorKind::PermissionDenied, "access denied");
        let context = json!({"file": "test.txt", "operation": "read"});
        log_error(&error, Some(context));
    }

    // Test performance logging
    #[test]
    fn test_log_performance_basic() {
        log_performance("test_operation", Duration::from_millis(100), None, None);
    }

    #[test]
    fn test_log_performance_with_context() {
        use serde_json::json;
        let context = json!({"files": 5, "lines": 100});
        log_performance("test_op", Duration::from_secs(1), Some(context), None);
    }

    #[test]
    fn test_log_performance_with_tags() {
        let mut tags = HashMap::new();
        tags.insert("command".to_string(), "commit".to_string());
        tags.insert("repo".to_string(), "test".to_string());
        log_performance("commit_op", Duration::from_millis(500), None, Some(tags));
    }

    // Test message logging
    #[test]
    fn test_log_message_basic() {
        log_message("test message", "info", None);
    }

    #[test]
    fn test_log_message_with_context() {
        use serde_json::json;
        let context = json!({"user": "test", "action": "login"});
        log_message("user logged in", "info", Some(context));
    }

    #[test]
    fn test_log_message_warning() {
        log_message("warning message", "warning", None);
    }

    // Test metrics logging
    #[test]
    fn test_log_metrics_empty() {
        log_metrics(vec![]);
    }

    // Test constants
    #[test]
    fn test_max_metrics_per_envelope() {
        assert_eq!(MAX_METRICS_PER_ENVELOPE, 250);
    }

    // is_mock_ai tests
    use crate::metrics::{
        MOCK_AI_TOOL, attrs::attr_pos, events::committed_pos, types::SparseArray,
    };
    use serde_json::Value;

    fn event_with_tool_attr(tool: &str) -> MetricEvent {
        let mut attrs: SparseArray = SparseArray::new();
        attrs.insert(attr_pos::TOOL.to_string(), Value::String(tool.to_string()));
        MetricEvent {
            timestamp: 0,
            event_id: 2,
            values: SparseArray::new(),
            attrs,
        }
    }

    fn event_with_tool_model_pairs(pairs: Vec<&str>) -> MetricEvent {
        let mut values: SparseArray = SparseArray::new();
        values.insert(
            committed_pos::TOOL_MODEL_PAIRS.to_string(),
            Value::Array(
                pairs
                    .into_iter()
                    .map(|s| Value::String(s.to_string()))
                    .collect(),
            ),
        );
        MetricEvent {
            timestamp: 0,
            event_id: 1,
            values,
            attrs: SparseArray::new(),
        }
    }

    #[test]
    fn test_is_mock_ai_true_for_tool_attr() {
        assert!(super::is_mock_ai(&event_with_tool_attr(MOCK_AI_TOOL)));
    }

    #[test]
    fn test_is_mock_ai_false_for_other_tool_attr() {
        assert!(!super::is_mock_ai(&event_with_tool_attr("claude-code")));
    }

    #[test]
    fn test_is_mock_ai_true_for_committed_pairs() {
        assert!(super::is_mock_ai(&event_with_tool_model_pairs(vec![
            "all",
            "mock_ai::unknown",
        ])));
    }

    #[test]
    fn test_is_mock_ai_false_for_other_committed_pairs() {
        assert!(!super::is_mock_ai(&event_with_tool_model_pairs(vec![
            "all",
            "claude-code::claude-sonnet-4-20250514",
        ])));
    }

    #[test]
    fn test_is_mock_ai_false_for_empty_event() {
        let event = MetricEvent {
            timestamp: 0,
            event_id: 1,
            values: SparseArray::new(),
            attrs: SparseArray::new(),
        };
        assert!(!super::is_mock_ai(&event));
    }
}
