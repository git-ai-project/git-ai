use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub mod flush;
pub mod wrapper_performance_targets;

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
}

#[derive(Clone)]
enum LogEnvelope {
    Error(ErrorEnvelope),
    Performance(PerformanceEnvelope),
    #[allow(dead_code)]
    Message(MessageEnvelope),
}

impl LogEnvelope {
    fn to_json(&self) -> Option<serde_json::Value> {
        match self {
            LogEnvelope::Error(e) => serde_json::to_value(e).ok(),
            LogEnvelope::Performance(p) => serde_json::to_value(p).ok(),
            LogEnvelope::Message(m) => serde_json::to_value(m).ok(),
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

fn get_observability() -> &'static Mutex<ObservabilityInner> {
    OBSERVABILITY.get_or_init(|| {
        Mutex::new(ObservabilityInner {
            mode: LogMode::Buffered(Vec::new()),
        })
    })
}

/// Set the repository context and flush buffered events to disk
/// Should be called once Repository is available
pub fn set_repo_context(repo: &crate::git::repository::Repository) {
    let log_path = repo
        .storage
        .logs
        .join(format!("{}.log", std::process::id()));

    let mut obs = get_observability().lock().unwrap();

    // Get buffered events
    let buffered_events = match &obs.mode {
        LogMode::Buffered(events) => events.clone(),
        LogMode::Disk(_) => return, // Already set, ignore
    };

    // Switch to disk mode
    obs.mode = LogMode::Disk(log_path.clone());
    drop(obs); // Release lock before writing

    // Flush buffered events to disk
    if !buffered_events.is_empty() {
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&log_path)
        {
            for envelope in buffered_events {
                if let Some(json) = envelope.to_json() {
                    let _ = writeln!(file, "{}", json.to_string());
                }
            }
        }
    }
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

            if let Some(json) = envelope.to_json() {
                if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
                    let _ = writeln!(file, "{}", json.to_string());
                }
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
pub fn log_performance(operation: &str, duration: Duration, context: Option<serde_json::Value>) {
    let envelope = PerformanceEnvelope {
        event_type: "performance".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        operation: operation.to_string(),
        duration_ms: duration.as_millis(),
        context,
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
    // Always spawn flush process - it will handle OSS/Enterprise DSN logic
    // and cleanup when telemetry_oss is "off"
    use std::process::Command;

    if let Ok(exe) = crate::utils::current_git_ai_exe() {
        let _ = Command::new(exe)
            .arg("flush-logs")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}
