//! Gemini agent implementation with sweep discovery.

use crate::metrics::events::AgentTraceValues;
use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy, TranscriptFormat};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{TimestampWatermark, WatermarkStrategy, WatermarkType};
use chrono::{DateTime, Utc};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Gemini agent that discovers conversations from Gemini session storage.
pub struct GeminiAgent;

impl GeminiAgent {
    /// Scan for Gemini session files in standard locations.
    ///
    /// Searches `~/.gemini/sessions/` recursively for `*.json` files.
    fn scan_session_files() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        if let Some(sessions_dir) = dirs::home_dir().map(|p| p.join(".gemini/sessions"))
            && sessions_dir.exists()
        {
            Self::scan_json_recursive(&sessions_dir, &mut paths);
        }

        paths
    }

    /// Recursively scan directory for `*.json` files.
    fn scan_json_recursive(dir: &Path, paths: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::scan_json_recursive(&path, paths);
            } else if path.is_file() && path.extension().map(|ext| ext == "json").unwrap_or(false) {
                paths.push(path);
            }
        }
    }

    /// Extract session ID from a Gemini session file path.
    ///
    /// Session ID format: `gemini:{file_stem}`
    fn extract_session_id(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| format!("gemini:{}", s))
    }
}

impl Agent for GeminiAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        let paths = Self::scan_session_files();
        let mut sessions = Vec::new();

        for path in paths {
            let Some(session_id) = Self::extract_session_id(&path) else {
                continue;
            };

            let session = DiscoveredSession {
                session_id,
                agent_type: "gemini".to_string(),
                transcript_path: path,
                transcript_format: TranscriptFormat::GeminiJson,
                watermark_type: WatermarkType::Timestamp,
                initial_watermark: Box::new(TimestampWatermark::new(DateTime::<Utc>::UNIX_EPOCH)),
                model: None,
                tool: Some("Gemini".to_string()),
                external_thread_id: None,
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
        // Downcast watermark to TimestampWatermark
        let ts_watermark = watermark
            .as_any()
            .downcast_ref::<TimestampWatermark>()
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "Gemini reader requires TimestampWatermark, got incompatible type for session {}",
                    session_id
                ),
            })?;

        let watermark_timestamp = ts_watermark.0;

        // Read the entire file
        let content = std::fs::read_to_string(path).map_err(|e| {
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

        // Parse the JSON
        let parsed: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| TranscriptError::Parse {
                line: 0,
                message: format!("Invalid JSON in {}: {}", path.display(), e),
            })?;

        // Get messages array
        let messages = parsed
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "Missing 'messages' array in Gemini session file: {}",
                    path.display()
                ),
            })?;

        let mut events = Vec::new();
        let mut model: Option<String> = None;
        let mut max_timestamp = watermark_timestamp;

        for message in messages {
            let msg_type = match message.get("type").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => continue,
            };

            // Parse timestamp if available
            let parsed_dt = message
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));

            // Filter by watermark: only emit events with timestamp strictly greater than watermark.
            // Events without a parseable timestamp are always emitted (not filtered).
            if let Some(dt) = parsed_dt {
                if dt <= watermark_timestamp {
                    continue;
                }
                // Update max timestamp
                if dt > max_timestamp {
                    max_timestamp = dt;
                }
            }

            let event_ts_epoch = parsed_dt.map(|dt| dt.timestamp() as u64);

            match msg_type {
                "user" => {
                    if let Some(content_str) = message.get("content").and_then(|v| v.as_str()) {
                        let mut event = AgentTraceValues::new()
                            .event_type("user_message")
                            .prompt_text(content_str);

                        if let Some(ts) = event_ts_epoch {
                            event = event.event_ts(ts);
                        }

                        events.push(event);
                    }
                }
                "gemini" => {
                    // Extract model (first gemini message with model wins)
                    if model.is_none()
                        && let Some(m) = message.get("model").and_then(|v| v.as_str())
                    {
                        model = Some(m.to_string());
                    }

                    // Assistant message
                    if let Some(content_str) = message.get("content").and_then(|v| v.as_str()) {
                        let mut event = AgentTraceValues::new()
                            .event_type("assistant_message")
                            .response_text(content_str);

                        if let Some(ts) = event_ts_epoch {
                            event = event.event_ts(ts);
                        }

                        events.push(event);
                    }

                    // Tool calls
                    if let Some(tool_calls) = message.get("toolCalls").and_then(|v| v.as_array()) {
                        for tool_call in tool_calls {
                            if let Some(name) = tool_call.get("name").and_then(|v| v.as_str()) {
                                let mut event = AgentTraceValues::new()
                                    .event_type("tool_use")
                                    .tool_name(name);

                                if let Some(ts) = event_ts_epoch {
                                    event = event.event_ts(ts);
                                }

                                events.push(event);
                            }
                        }
                    }
                }
                _ => {} // Skip unknown types
            }
        }

        let new_watermark = Box::new(TimestampWatermark::new(max_timestamp));

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
        let agent = GeminiAgent;
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
            "messages": [
                {"type": "user", "content": "Hello", "timestamp": "2025-01-01T00:00:00Z"},
                {"type": "gemini", "content": "Hi there", "model": "gemini-pro", "timestamp": "2025-01-01T00:00:01Z"}
            ]
        });

        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", json).unwrap();
        file.flush().unwrap();

        let agent = GeminiAgent;
        let watermark = Box::new(TimestampWatermark::new(DateTime::<Utc>::UNIX_EPOCH));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, Some("gemini-pro".to_string()));
    }

    #[test]
    fn test_read_incremental_filters_by_watermark() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = serde_json::json!({
            "messages": [
                {"type": "user", "content": "Old message", "timestamp": "2025-01-01T00:00:00Z"},
                {"type": "gemini", "content": "New message", "timestamp": "2025-01-01T00:01:00Z"}
            ]
        });

        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", json).unwrap();
        file.flush().unwrap();

        let agent = GeminiAgent;
        // Set watermark to after the first message
        let ts = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let watermark = Box::new(TimestampWatermark::new(ts));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        // Only the second message should be returned (strictly greater than watermark)
        assert_eq!(result.events.len(), 1);
    }
}
