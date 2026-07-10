use std::collections::HashMap;
use std::time::Duration;

use crate::daemon::daemon_log_layer::{
    MAX_MESSAGE_LENGTH, bounded_copy, bounded_display_string, sanitize_log_string,
};
use crate::metrics::MetricEvent;

pub mod performance_targets;

/// Maximum events per metrics envelope
pub const MAX_METRICS_PER_ENVELOPE: usize = 1000;

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

fn telemetry_error_message(error: &dyn std::error::Error) -> String {
    let bounded = bounded_display_string(error, MAX_MESSAGE_LENGTH);
    sanitize_log_string(&bounded, MAX_MESSAGE_LENGTH)
}

fn telemetry_log_message(message: &str) -> String {
    let bounded = bounded_copy(message, MAX_MESSAGE_LENGTH);
    sanitize_log_string(&bounded, MAX_MESSAGE_LENGTH)
}

/// Log an error to Sentry (via daemon telemetry worker)
pub fn log_error(error: &dyn std::error::Error, context: Option<serde_json::Value>) {
    let envelope = crate::daemon::TelemetryEnvelope::Error {
        timestamp: chrono::Utc::now().to_rfc3339(),
        message: telemetry_error_message(error),
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
        message: telemetry_log_message(message),
        level: level.to_string(),
        context,
    };
    submit_telemetry_envelope(vec![envelope]);
}

/// Log a batch of metric events (via daemon telemetry worker).
///
/// Events are batched into envelopes of up to 1000 events each.
pub fn log_metrics(events: Vec<MetricEvent>) {
    #[cfg(any(test, feature = "test-support"))]
    {
        if std::env::var_os("GIT_AI_TEST_METRICS_DB_PATH").is_none() {
            return;
        }
    }

    if events.is_empty() {
        return;
    }

    // Consume owned chunks so large metric payloads are not cloned before submission.
    let mut events = events.into_iter();
    loop {
        let chunk = events
            .by_ref()
            .take(MAX_METRICS_PER_ENVELOPE)
            .collect::<Vec<_>>();
        if chunk.is_empty() {
            break;
        }
        let envelope = crate::daemon::TelemetryEnvelope::Metrics { events: chunk };
        submit_telemetry_envelope(vec![envelope]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fmt;
    use std::time::Duration;

    struct SecretError(String);

    impl fmt::Display for SecretError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    impl fmt::Debug for SecretError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.debug_tuple("SecretError").field(&self.0).finish()
        }
    }

    impl std::error::Error for SecretError {}

    #[test]
    fn telemetry_error_message_is_redacted_and_hard_bounded() {
        let secret = "sk_test_4eC39HqLyjWDarjtT1zdp7dc";
        let error = SecretError(format!(
            "{} token={secret} trailing data",
            "x".repeat(MAX_MESSAGE_LENGTH)
        ));

        let message = telemetry_error_message(&error);

        assert_eq!(message.chars().count(), MAX_MESSAGE_LENGTH);
        assert!(!message.contains(secret));
    }

    #[test]
    fn telemetry_log_message_is_redacted_and_hard_bounded() {
        let secret = "sk_test_4eC39HqLyjWDarjtT1zdp7dc";
        let raw = format!(
            "{} token={secret} trailing data",
            "x".repeat(MAX_MESSAGE_LENGTH)
        );

        let message = telemetry_log_message(&raw);

        assert_eq!(message.chars().count(), MAX_MESSAGE_LENGTH);
        assert!(!message.contains(secret));
    }

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
        assert_eq!(MAX_METRICS_PER_ENVELOPE, 1000);
    }
}
