//! Amp agent implementation with sweep discovery.

use crate::metrics::events::AgentTraceValues;
use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy, TranscriptFormat};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{RecordIndexWatermark, WatermarkStrategy, WatermarkType};
use chrono::DateTime;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct AmpThread {
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    messages: Vec<AmpThreadMessage>,
}

#[derive(Debug, Deserialize)]
struct AmpThreadMessage {
    role: String,
    #[serde(default)]
    content: Vec<AmpThreadContent>,
    #[serde(default)]
    meta: Option<AmpMessageMeta>,
    #[serde(default)]
    usage: Option<AmpMessageUsage>,
}

#[derive(Debug, Deserialize)]
struct AmpMessageMeta {
    #[serde(rename = "sentAt")]
    sent_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AmpMessageUsage {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AmpThreadContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[allow(dead_code)]
        id: String,
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(rename = "toolUseID")]
        #[allow(dead_code)]
        tool_use_id: String,
    },
    #[serde(other)]
    Unknown,
}

/// Amp agent that discovers conversations from Amp thread JSON files.
pub struct AmpAgent;

impl AmpAgent {
    /// Returns the path to Amp thread files.
    ///
    /// Checks `GIT_AI_AMP_THREADS_PATH` env var first, then falls back to
    /// platform-specific default locations.
    pub fn amp_threads_path() -> Result<PathBuf, TranscriptError> {
        if let Ok(path) = std::env::var("GIT_AI_AMP_THREADS_PATH") {
            return Ok(PathBuf::from(path));
        }

        #[cfg(target_os = "macos")]
        {
            if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
                return Ok(PathBuf::from(xdg).join("amp/threads"));
            }
            if let Some(home) = dirs::home_dir() {
                return Ok(home.join(".local/share/amp/threads"));
            }
        }

        #[cfg(target_os = "linux")]
        {
            if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
                return Ok(PathBuf::from(xdg).join("amp/threads"));
            }
            if let Some(home) = dirs::home_dir() {
                return Ok(home.join(".local/share/amp/threads"));
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                return Ok(PathBuf::from(local).join("amp/threads"));
            }
            if let Ok(appdata) = std::env::var("APPDATA") {
                return Ok(PathBuf::from(appdata).join("amp/threads"));
            }
        }

        Err(TranscriptError::Fatal {
            message: "Could not determine Amp threads path".to_string(),
        })
    }
}

impl Agent for AmpAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        let threads_dir = match Self::amp_threads_path() {
            Ok(p) => p,
            Err(_) => return Ok(Vec::new()),
        };

        if !threads_dir.exists() {
            return Ok(Vec::new());
        }

        let entries = fs::read_dir(&threads_dir).map_err(|e| TranscriptError::Transient {
            message: format!("Failed to read Amp threads directory: {}", e),
            retry_after: Duration::from_secs(30),
        })?;

        let mut sessions = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let Some(file_stem) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
            else {
                continue;
            };

            let session_id = format!("amp:{}", file_stem);

            // Use file stem as external_thread_id to avoid parsing every file during discovery
            let session = DiscoveredSession {
                session_id,
                agent_type: "amp".to_string(),
                transcript_path: path,
                transcript_format: TranscriptFormat::AmpThreadJson,
                watermark_type: WatermarkType::RecordIndex,
                initial_watermark: Box::new(RecordIndexWatermark::new(0)),
                model: None,
                tool: Some("Amp".to_string()),
                external_thread_id: Some(file_stem),
            };

            sessions.push(session);
        }

        Ok(sessions)
    }

    fn read_incremental(
        &self,
        path: &Path,
        watermark: Box<dyn WatermarkStrategy>,
        session_id: &str,
    ) -> Result<TranscriptBatch, TranscriptError> {
        // Downcast watermark to RecordIndexWatermark
        let record_watermark = watermark
            .as_any()
            .downcast_ref::<RecordIndexWatermark>()
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "Amp reader requires RecordIndexWatermark, got incompatible type for session {}",
                    session_id
                ),
            })?;

        let skip_count = record_watermark.0 as usize;

        // Read and parse the JSON file
        let content = fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TranscriptError::Fatal {
                    message: format!("Transcript file not found: {}", path.display()),
                }
            } else if e.kind() == std::io::ErrorKind::PermissionDenied {
                TranscriptError::Fatal {
                    message: format!("Permission denied reading transcript: {}", path.display()),
                }
            } else {
                TranscriptError::Transient {
                    message: format!("Failed to read transcript file: {}", e),
                    retry_after: Duration::from_secs(5),
                }
            }
        })?;

        let thread: AmpThread =
            serde_json::from_str(&content).map_err(|e| TranscriptError::Parse {
                line: 0,
                message: format!("Invalid JSON in {}: {}", path.display(), e),
            })?;

        let total_messages = thread.messages.len();
        let mut events = Vec::new();
        let mut model: Option<String> = None;

        // Skip first `skip_count` messages (already processed)
        for msg in thread.messages.iter().skip(skip_count) {
            // Extract model from assistant messages' usage.model (first non-empty wins)
            if model.is_none()
                && msg.role == "assistant"
                && let Some(ref usage) = msg.usage
                && let Some(ref m) = usage.model
                && !m.is_empty()
            {
                model = Some(m.clone());
            }

            // Compute timestamp
            let timestamp_opt: Option<u64> = match msg.role.as_str() {
                "user" => msg.meta.as_ref().and_then(|meta| {
                    meta.sent_at.and_then(|ms| {
                        DateTime::from_timestamp_millis(ms).map(|dt| dt.timestamp() as u64)
                    })
                }),
                "assistant" => msg.usage.as_ref().and_then(|usage| {
                    usage.timestamp.as_ref().and_then(|ts| {
                        DateTime::parse_from_rfc3339(ts)
                            .ok()
                            .map(|dt| dt.timestamp() as u64)
                    })
                }),
                _ => None,
            };

            // Process each content item
            for content_item in &msg.content {
                match content_item {
                    AmpThreadContent::Text { text } => {
                        if text.trim().is_empty() {
                            continue;
                        }
                        let event = match msg.role.as_str() {
                            "user" => {
                                let e = AgentTraceValues::new()
                                    .event_type("user_message")
                                    .prompt_text(text);
                                if let Some(ts) = timestamp_opt {
                                    e.event_ts(ts)
                                } else {
                                    e
                                }
                            }
                            "assistant" => {
                                let e = AgentTraceValues::new()
                                    .event_type("assistant_message")
                                    .response_text(text);
                                if let Some(ts) = timestamp_opt {
                                    e.event_ts(ts)
                                } else {
                                    e
                                }
                            }
                            _ => continue,
                        };
                        events.push(event);
                    }
                    AmpThreadContent::Thinking { thinking } => {
                        if msg.role == "assistant" && !thinking.trim().is_empty() {
                            let event = AgentTraceValues::new()
                                .event_type("assistant_thinking")
                                .response_text(thinking);
                            let event = if let Some(ts) = timestamp_opt {
                                event.event_ts(ts)
                            } else {
                                event
                            };
                            events.push(event);
                        }
                    }
                    AmpThreadContent::ToolUse { name, .. } => {
                        if msg.role == "assistant" {
                            let event = AgentTraceValues::new()
                                .event_type("tool_use")
                                .tool_name(name);
                            let event = if let Some(ts) = timestamp_opt {
                                event.event_ts(ts)
                            } else {
                                event
                            };
                            events.push(event);
                        }
                    }
                    AmpThreadContent::ToolResult { .. } | AmpThreadContent::Unknown => {}
                }
            }
        }

        let new_watermark = Box::new(RecordIndexWatermark::new(total_messages as u64));

        Ok(TranscriptBatch {
            events,
            model,
            new_watermark,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sweep_strategy() {
        let agent = AmpAgent;
        assert_eq!(
            agent.sweep_strategy(),
            SweepStrategy::Periodic(Duration::from_secs(30 * 60))
        );
    }

    #[test]
    fn test_read_incremental_basic() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = serde_json::json!({
            "id": "thread-123",
            "messages": [
                {
                    "role": "user",
                    "content": [{"type": "text", "text": "Hello"}],
                    "meta": {"sentAt": 1704067200000i64}
                },
                {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "Hi"}],
                    "usage": {"model": "claude-sonnet-4-20250514", "timestamp": "2025-01-01T00:00:01Z"}
                }
            ]
        });

        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", json).unwrap();
        file.flush().unwrap();

        let agent = AmpAgent;
        let watermark = Box::new(RecordIndexWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, Some("claude-sonnet-4-20250514".to_string()));
    }

    #[test]
    fn test_read_incremental_skips_processed() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = serde_json::json!({
            "id": "thread-123",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Old"}]},
                {"role": "user", "content": [{"type": "text", "text": "New"}]}
            ]
        });

        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", json).unwrap();
        file.flush().unwrap();

        let agent = AmpAgent;
        let watermark = Box::new(RecordIndexWatermark::new(1));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 1);
    }

    #[test]
    fn test_read_incremental_thinking_and_tool_use() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = serde_json::json!({
            "id": "thread-456",
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "Let me think..."},
                        {"type": "text", "text": "Here's the result"},
                        {"type": "tool_use", "id": "tu-1", "name": "bash", "input": {}}
                    ]
                }
            ]
        });

        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", json).unwrap();
        file.flush().unwrap();

        let agent = AmpAgent;
        let watermark = Box::new(RecordIndexWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 3); // thinking + text + tool_use
    }
}
