use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::metrics::MetricEvent;

pub mod flush;
pub mod wrapper_performance_targets;

/// Maximum events per metrics envelope
pub const MAX_METRICS_PER_ENVELOPE: usize = 250;

#[derive(Serialize, Deserialize, Clone)]
struct ErrorEnvelope {
    #[serde(rename = "type")]
    event_type: String,
    timestamp: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Clone)]
struct MessageEnvelope {
    #[serde(rename = "type")]
    event_type: String,
    timestamp: String,
    message: String,
    level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Clone)]
struct PerformanceEnvelope {
    #[serde(rename = "type")]
    event_type: String,
    timestamp: String,
    operation: String,
    duration_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<HashMap<String, String>>,
}

#[derive(Serialize, Deserialize, Clone)]
struct MetricsEnvelope {
    #[serde(rename = "type")]
    event_type: String,
    timestamp: String,
    version: u8,
    events: Vec<MetricEvent>,
}

#[derive(Clone)]
enum LogEnvelope {
    Error(ErrorEnvelope),
    Performance(PerformanceEnvelope),
    #[allow(dead_code)]
    Message(MessageEnvelope),
    #[allow(dead_code)]
    Metrics(MetricsEnvelope),
}

impl LogEnvelope {
    fn to_json(&self) -> Option<serde_json::Value> {
        match self {
            LogEnvelope::Error(e) => serde_json::to_value(e).ok(),
            LogEnvelope::Performance(p) => serde_json::to_value(p).ok(),
            LogEnvelope::Message(m) => serde_json::to_value(m).ok(),
            LogEnvelope::Metrics(m) => serde_json::to_value(m).ok(),
        }
    }
}

enum LogMode {
    Buffered(Vec<LogEnvelope>),
    Disk(PathBuf),
}

struct ObservabilityInner {
    mode: LogMode,
}

static OBSERVABILITY: OnceLock<Mutex<ObservabilityInner>> = OnceLock::new();
const ENV_FLUSH_LOGS_WORKER: &str = "GIT_AI_FLUSH_LOGS_WORKER";

fn get_observability() -> &'static Mutex<ObservabilityInner> {
    OBSERVABILITY.get_or_init(|| {
        // Initialize directly in Disk mode with global logs path
        // All logs go to ~/.git-ai/internal/logs/{PID}.log
        let mode = if let Some(home) = dirs::home_dir() {
            let logs_dir = home.join(".git-ai").join("internal").join("logs");
            if std::fs::create_dir_all(&logs_dir).is_ok() {
                LogMode::Disk(logs_dir.join(format!("{}.log", std::process::id())))
            } else {
                LogMode::Buffered(Vec::new())
            }
        } else {
            LogMode::Buffered(Vec::new())
        };
        Mutex::new(ObservabilityInner { mode })
    })
}

/// Append an envelope (buffer if no repo context, write to disk if context set)
fn append_envelope(envelope: LogEnvelope) {
    let mut obs = get_observability().lock().unwrap();

    match &mut obs.mode {
        LogMode::Buffered(buffer) => {
            buffer.push(envelope);
        }
        LogMode::Disk(log_path) => {
            let log_path = log_path.clone();
            drop(obs); // Release lock before file I/O

            if let Some(json) = envelope.to_json()
                && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path)
            {
                let _ = writeln!(file, "{}", json);
            }
        }
    }
}

/// Log an error to Sentry
pub fn log_error(error: &dyn std::error::Error, context: Option<serde_json::Value>) {
    let envelope = ErrorEnvelope {
        event_type: "error".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        message: error.to_string(),
        context,
    };

    append_envelope(LogEnvelope::Error(envelope));
}

/// Log a performance metric to Sentry
pub fn log_performance(
    operation: &str,
    duration: Duration,
    context: Option<serde_json::Value>,
    tags: Option<HashMap<String, String>>,
) {
    let envelope = PerformanceEnvelope {
        event_type: "performance".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        operation: operation.to_string(),
        duration_ms: duration.as_millis(),
        context,
        tags,
    };

    append_envelope(LogEnvelope::Performance(envelope));
}

/// Log a message to Sentry (info, warning, etc.)
#[allow(dead_code)]
pub fn log_message(message: &str, level: &str, context: Option<serde_json::Value>) {
    let envelope = MessageEnvelope {
        event_type: "message".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        message: message.to_string(),
        level: level.to_string(),
        context,
    };

    append_envelope(LogEnvelope::Message(envelope));
}

/// Spawn a background process to flush logs to Sentry
pub fn spawn_background_flush() {
    // Skip flush in test builds to prevent race conditions during test cleanup.
    // Tests spawn git-ai as a subprocess which calls this function. If the background
    // flush process is still starting when TestRepo::drop() runs, file handles may
    // remain open causing "Directory not empty" errors. Tests set GIT_AI_TEST_DB_PATH
    // to isolate their database, so we use that as the test detection mechanism.
    // This check is compiled out of release builds since tests only run in debug mode.
    #[cfg(debug_assertions)]
    if std::env::var("GIT_AI_TEST_DB_PATH").is_ok() {
        return;
    }

    if !should_spawn_background_flush() {
        return;
    }

    let _ = crate::utils::spawn_internal_git_ai_subcommand(
        "flush-logs",
        &[],
        ENV_FLUSH_LOGS_WORKER,
        &[],
    );
}

/// Debounce background flushes to avoid process/request storms when checkpoints
/// run in quick succession.
fn should_spawn_background_flush() -> bool {
    const MIN_FLUSH_INTERVAL_SECS: u64 = 60;

    let Some(home) = dirs::home_dir() else {
        return true;
    };
    let internal_dir = home.join(".git-ai").join("internal");
    let _ = std::fs::create_dir_all(&internal_dir);

    let marker = internal_dir.join("last_flush_trigger_ts");
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if let Ok(previous) = std::fs::read_to_string(&marker)
        && let Ok(previous_secs) = previous.trim().parse::<u64>()
        && now_secs.saturating_sub(previous_secs) < MIN_FLUSH_INTERVAL_SECS
    {
        return false;
    }

    let _ = std::fs::write(&marker, now_secs.to_string());
    true
}

/// Log a batch of metric events to the observability log file.
///
/// Events are batched into envelopes of up to 250 events each.
/// The flush-logs command will then upload them to the API or
/// store them in SQLite for later upload.
pub fn log_metrics(
    #[cfg_attr(any(test, feature = "test-support"), allow(unused))] events: Vec<MetricEvent>,
) {
    #[cfg(any(test, feature = "test-support"))]
    return;

    #[cfg(not(any(test, feature = "test-support")))]
    {
        if events.is_empty() {
            return;
        }

        // Split into chunks of MAX_METRICS_PER_ENVELOPE
        for chunk in events.chunks(MAX_METRICS_PER_ENVELOPE) {
            let envelope = MetricsEnvelope {
                event_type: "metrics".to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                version: crate::metrics::METRICS_API_VERSION,
                events: chunk.to_vec(),
            };

            append_envelope(LogEnvelope::Metrics(envelope));
        }
    }
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

    // Test spawn_background_flush
    #[test]
    fn test_spawn_background_flush_no_panic() {
        // In test mode, this should exit early due to GIT_AI_TEST_DB_PATH check
        spawn_background_flush();
    }

    // Test constants
    #[test]
    fn test_max_metrics_per_envelope() {
        assert_eq!(MAX_METRICS_PER_ENVELOPE, 250);
    }

    // Test envelope serialization
    #[test]
    fn test_error_envelope_to_json() {
        let envelope = ErrorEnvelope {
            event_type: "error".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message: "test error".to_string(),
            context: None,
        };
        let log_envelope = LogEnvelope::Error(envelope);
        let json = log_envelope.to_json();
        assert!(json.is_some());
    }

    #[test]
    fn test_performance_envelope_to_json() {
        let envelope = PerformanceEnvelope {
            event_type: "performance".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            operation: "test_op".to_string(),
            duration_ms: 100,
            context: None,
            tags: None,
        };
        let log_envelope = LogEnvelope::Performance(envelope);
        let json = log_envelope.to_json();
        assert!(json.is_some());
    }

    #[test]
    fn test_message_envelope_to_json() {
        let envelope = MessageEnvelope {
            event_type: "message".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message: "test message".to_string(),
            level: "info".to_string(),
            context: None,
        };
        let log_envelope = LogEnvelope::Message(envelope);
        let json = log_envelope.to_json();
        assert!(json.is_some());
    }

    #[test]
    fn test_metrics_envelope_to_json() {
        // Test empty metrics envelope
        let envelope = MetricsEnvelope {
            event_type: "metrics".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            version: 1,
            events: vec![],
        };
        let log_envelope = LogEnvelope::Metrics(envelope);
        let json = log_envelope.to_json();
        assert!(json.is_some());
    }
}
