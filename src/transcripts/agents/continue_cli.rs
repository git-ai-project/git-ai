//! Continue CLI agent implementation with sweep discovery.

use crate::metrics::events::AgentTraceValues;
use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy, TranscriptFormat};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{RecordIndexWatermark, WatermarkStrategy, WatermarkType};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Continue CLI agent that reads Continue JSON transcript files.
///
/// Uses `RecordIndexWatermark` because the format has no timestamps at all.
/// We track how many history entries we've already processed and skip that
/// many on re-read.
pub struct ContinueAgent;

impl ContinueAgent {
    /// Scan for Continue session files in `~/.continue/sessions/**/*.json`.
    fn scan_session_files() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        if let Some(home) = dirs::home_dir() {
            let pattern = home
                .join(".continue/sessions/**/*.json")
                .to_string_lossy()
                .to_string();

            if let Ok(entries) = glob::glob(&pattern) {
                for entry in entries.flatten() {
                    if entry.is_file() {
                        paths.push(entry);
                    }
                }
            }
        }

        paths
    }

    /// Extract session ID from a Continue session file path.
    ///
    /// Continue files are typically named like: `<session-name>.json`
    fn extract_session_id(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| format!("continue:{}", s))
    }
}

impl Agent for ContinueAgent {
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
                agent_type: "continue".to_string(),
                transcript_path: path,
                transcript_format: TranscriptFormat::ContinueJson,
                watermark_type: WatermarkType::RecordIndex,
                initial_watermark: Box::new(RecordIndexWatermark::new(0)),
                model: None,
                tool: Some("Continue".to_string()),
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
        // Downcast watermark to RecordIndexWatermark
        let record_watermark = watermark
            .as_any()
            .downcast_ref::<RecordIndexWatermark>()
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "Continue reader requires RecordIndexWatermark, got incompatible type for session {}",
                    session_id
                ),
            })?;

        let already_processed = record_watermark.0;

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

        // Parse JSON
        let parsed: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| TranscriptError::Parse {
                line: 0,
                message: format!("Invalid JSON in {}: {}", path.display(), e),
            })?;

        // Get the history array (fatal if missing)
        let history = parsed
            .get("history")
            .and_then(|v| v.as_array())
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "Missing 'history' array in Continue transcript: {}",
                    path.display()
                ),
            })?;

        let total_history_length = history.len() as u64;

        // Skip already-processed entries (exactly-once guarantee)
        let new_entries = if already_processed as usize >= history.len() {
            &[][..]
        } else {
            &history[already_processed as usize..]
        };

        let mut events = Vec::new();

        for history_item in new_entries {
            let message = history_item.get("message");
            let role = message.and_then(|m| m.get("role")).and_then(|v| v.as_str());

            match role {
                Some("user") => {
                    // User message: content is a string
                    if let Some(text) = message
                        .and_then(|m| m.get("content"))
                        .and_then(|v| v.as_str())
                    {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            let event = AgentTraceValues::new()
                                .event_type("user_message")
                                .prompt_text(trimmed);

                            events.push(event);
                        }
                    }
                }
                Some("assistant") => {
                    // Assistant message: content can be String or Array
                    if let Some(content) = message.and_then(|m| m.get("content")) {
                        match content {
                            serde_json::Value::String(text) => {
                                let trimmed = text.trim();
                                if !trimmed.is_empty() {
                                    let event = AgentTraceValues::new()
                                        .event_type("assistant_message")
                                        .response_text(trimmed);

                                    events.push(event);
                                }
                            }
                            serde_json::Value::Array(parts) => {
                                for part in parts {
                                    if let Some(text) = part.as_str() {
                                        let trimmed = text.trim();
                                        if !trimmed.is_empty() {
                                            let event = AgentTraceValues::new()
                                                .event_type("assistant_message")
                                                .response_text(trimmed);

                                            events.push(event);
                                        }
                                    } else if let Some(text) =
                                        part.get("text").and_then(|v| v.as_str())
                                    {
                                        let trimmed = text.trim();
                                        if !trimmed.is_empty() {
                                            let event = AgentTraceValues::new()
                                                .event_type("assistant_message")
                                                .response_text(trimmed);

                                            events.push(event);
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }

                    // Check contextItems on the history item (NOT the message)
                    if let Some(context_items) =
                        history_item.get("contextItems").and_then(|v| v.as_array())
                    {
                        for item in context_items {
                            if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                                let event = AgentTraceValues::new()
                                    .event_type("tool_use")
                                    .tool_name(name);

                                events.push(event);
                            }
                        }
                    }
                }
                _ => {} // Skip unknown roles
            }
        }

        // New watermark = total history length
        let new_watermark = Box::new(RecordIndexWatermark::new(total_history_length));

        Ok(TranscriptBatch {
            events,
            model: None,
            new_watermark,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sweep_strategy() {
        let agent = ContinueAgent;
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
            "history": [
                {"message": {"role": "user", "content": "Hello"}},
                {"message": {"role": "assistant", "content": "Hi there"}}
            ]
        });

        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", json).unwrap();
        file.flush().unwrap();

        let agent = ContinueAgent;
        let watermark = Box::new(RecordIndexWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, None);
    }

    #[test]
    fn test_read_incremental_skips_already_processed() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = serde_json::json!({
            "history": [
                {"message": {"role": "user", "content": "Old"}},
                {"message": {"role": "assistant", "content": "Old reply"}},
                {"message": {"role": "user", "content": "New"}}
            ]
        });

        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", json).unwrap();
        file.flush().unwrap();

        let agent = ContinueAgent;
        let watermark = Box::new(RecordIndexWatermark::new(2)); // Already processed 2
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 1); // Only the new message
    }

    #[test]
    fn test_read_incremental_with_context_items() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = serde_json::json!({
            "history": [
                {
                    "message": {"role": "assistant", "content": "Let me check"},
                    "contextItems": [
                        {"name": "file_reader", "content": "some data"}
                    ]
                }
            ]
        });

        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", json).unwrap();
        file.flush().unwrap();

        let agent = ContinueAgent;
        let watermark = Box::new(RecordIndexWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        // assistant_message + tool_use
        assert_eq!(result.events.len(), 2);
    }
}
