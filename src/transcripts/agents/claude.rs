//! Claude Code agent implementation with sweep discovery.

use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy, TranscriptFormat};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy, WatermarkType};
use chrono::{DateTime, Utc};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Claude Code agent that discovers conversations from Claude Code storage.
pub struct ClaudeAgent;

impl ClaudeAgent {
    /// Scan for Claude conversation files in standard locations.
    fn scan_conversation_files() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Standard locations for Claude Code transcripts
        let search_dirs = vec![
            // macOS/Linux Claude Code
            dirs::config_dir().map(|p| p.join("Claude/conversations")),
            // VS Code extension
            dirs::config_dir().map(|p| p.join("Code/User/globalStorage/claude/conversations")),
        ];

        for dir_opt in search_dirs {
            if let Some(dir) = dir_opt
                && dir.exists()
                && let Ok(entries) = fs::read_dir(&dir)
            {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file()
                        && path
                            .extension()
                            .map(|ext| ext == "jsonl")
                            .unwrap_or(false)
                    {
                        paths.push(path);
                    }
                }
            }
        }

        paths
    }

    /// Extract session ID from a Claude conversation file path.
    ///
    /// Claude files are typically named like: `conversation_<uuid>.jsonl`
    fn extract_session_id(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| format!("claude:{}", s))
    }

    /// Parse a Claude transcript file to extract metadata (model, timestamps).
    fn extract_metadata(path: &Path) -> (Option<String>, Option<DateTime<Utc>>) {
        use std::io::{BufRead, BufReader};

        let Ok(file) = fs::File::open(path) else {
            return (None, None);
        };

        let reader = BufReader::new(file);
        let mut model = None;
        let mut first_timestamp = None;

        // Read first few lines to extract metadata
        for line in reader.lines().take(10).flatten() {
            if line.trim().is_empty() {
                continue;
            }

            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) {
                // Extract model from assistant messages
                if model.is_none()
                    && entry["type"].as_str() == Some("assistant")
                    && let Some(model_str) = entry["message"]["model"].as_str()
                {
                    model = Some(model_str.to_string());
                }

                // Extract first timestamp
                if first_timestamp.is_none()
                    && let Some(ts_str) = entry["timestamp"].as_str()
                    && let Ok(ts) = DateTime::parse_from_rfc3339(ts_str)
                {
                    first_timestamp = Some(ts.with_timezone(&Utc));
                }

                // Stop if we have both
                if model.is_some() && first_timestamp.is_some() {
                    break;
                }
            }
        }

        (model, first_timestamp)
    }
}

impl Agent for ClaudeAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        // Poll every 30 minutes for new Claude conversations
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        let paths = Self::scan_conversation_files();
        let mut sessions = Vec::new();

        for path in paths {
            let Some(session_id) = Self::extract_session_id(&path) else {
                continue;
            };

            let (model, _first_timestamp) = Self::extract_metadata(&path);

            let session = DiscoveredSession {
                session_id,
                agent_type: "claude".to_string(),
                transcript_path: path,
                transcript_format: TranscriptFormat::ClaudeJsonl,
                watermark_type: WatermarkType::ByteOffset,
                initial_watermark: Box::new(ByteOffsetWatermark::new(0)),
                model,
                tool: Some("Claude Code".to_string()),
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
        // Migrated from formats/claude.rs (will be removed in Phase 9)
        use crate::metrics::events::AgentTraceValues;
        use std::fs::File;
        use std::io::{BufRead, BufReader, Seek, SeekFrom};

        // Downcast watermark to ByteOffsetWatermark
        let byte_watermark = watermark
            .as_any()
            .downcast_ref::<ByteOffsetWatermark>()
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "Claude reader requires ByteOffsetWatermark, got incompatible type for session {}",
                    session_id
                ),
            })?;

        let start_offset = byte_watermark.0;

        // Open file
        let file = File::open(path).map_err(|e| {
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
                    message: format!("Failed to open transcript file: {}", e),
                    retry_after: std::time::Duration::from_secs(5),
                }
            }
        })?;

        let mut reader = BufReader::new(file);

        // Seek to watermark position
        reader
            .seek(SeekFrom::Start(start_offset))
            .map_err(|e| TranscriptError::Transient {
                message: format!("Failed to seek to offset {}: {}", start_offset, e),
                retry_after: std::time::Duration::from_secs(5),
            })?;

        let mut events = Vec::new();
        let mut model = None;
        let mut current_offset = start_offset;
        let mut line_number = 0;

        // Read lines from watermark position
        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read = reader
                .read_line(&mut line)
                .map_err(|e| TranscriptError::Transient {
                    message: format!("I/O error reading line: {}", e),
                    retry_after: std::time::Duration::from_secs(5),
                })?;

            if bytes_read == 0 {
                // EOF
                break;
            }

            line_number += 1;

            // Update offset before processing (so we skip this line on next read even if parsing fails)
            current_offset += bytes_read as u64;

            // Skip empty lines
            if line.trim().is_empty() {
                continue;
            }

            // Parse JSONL entry
            let entry: serde_json::Value =
                serde_json::from_str(&line).map_err(|e| TranscriptError::Parse {
                    line: line_number,
                    message: format!("Invalid JSON in {}: {}", path.display(), e),
                })?;

            // Extract timestamp
            let timestamp_opt = entry["timestamp"].as_str().and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.timestamp() as u64)
            });

            // Extract model from assistant messages
            if model.is_none()
                && entry["type"].as_str() == Some("assistant")
                && let Some(model_str) = entry["message"]["model"].as_str()
            {
                model = Some(model_str.to_string());
            }

            // Extract events based on message type
            match entry["type"].as_str() {
                Some("user") => {
                    // User message - extract text content
                    let text = if let Some(content) = entry["message"]["content"].as_str() {
                        content.to_string()
                    } else if let Some(content_array) = entry["message"]["content"].as_array() {
                        // Handle content array - concatenate text blocks, skip tool_result
                        let mut texts = Vec::new();
                        for item in content_array {
                            if item["type"].as_str() == Some("tool_result") {
                                continue; // Skip system-generated tool results
                            }
                            if item["type"].as_str() == Some("text")
                                && let Some(text) = item["text"].as_str()
                                && !text.trim().is_empty()
                            {
                                texts.push(text.to_string());
                            }
                        }
                        texts.join("\n")
                    } else {
                        String::new()
                    };

                    if !text.trim().is_empty() {
                        let event = AgentTraceValues::new()
                            .event_type("user_message")
                            .prompt_text(text);

                        let event = if let Some(ts) = timestamp_opt {
                            event.event_ts(ts)
                        } else {
                            event
                        };

                        events.push(event);
                    }
                }
                Some("assistant") => {
                    // Assistant message - can contain text, thinking, and tool_use
                    if let Some(content_array) = entry["message"]["content"].as_array() {
                        for item in content_array {
                            match item["type"].as_str() {
                                Some("text") => {
                                    if let Some(text) = item["text"].as_str()
                                        && !text.trim().is_empty()
                                    {
                                        let event = AgentTraceValues::new()
                                            .event_type("assistant_message")
                                            .response_text(text);

                                        let event = if let Some(ts) = timestamp_opt {
                                            event.event_ts(ts)
                                        } else {
                                            event
                                        };

                                        events.push(event);
                                    }
                                }
                                Some("thinking") => {
                                    if let Some(thinking) = item["thinking"].as_str()
                                        && !thinking.trim().is_empty()
                                    {
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
                                Some("tool_use") => {
                                    if let Some(name) = item["name"].as_str() {
                                        let tool_use_id = item["id"].as_str().map(|s| s.to_string());

                                        let mut event = AgentTraceValues::new()
                                            .event_type("tool_use")
                                            .tool_name(name);

                                        if let Some(id) = tool_use_id {
                                            event = event.tool_use_id(id);
                                        }

                                        if let Some(ts) = timestamp_opt {
                                            event = event.event_ts(ts);
                                        }

                                        events.push(event);
                                    }
                                }
                                _ => {} // Skip unknown content types
                            }
                        }
                    }
                }
                _ => {} // Skip unknown message types
            }
        }

        // Create new watermark with updated offset
        let new_watermark = Box::new(ByteOffsetWatermark::new(current_offset));

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
    fn test_extract_session_id() {
        let path = PathBuf::from("/home/user/.config/Claude/conversations/conversation_abc-123.jsonl");
        let session_id = ClaudeAgent::extract_session_id(&path);
        assert_eq!(session_id, Some("claude:conversation_abc-123".to_string()));
    }

    #[test]
    fn test_sweep_strategy() {
        let agent = ClaudeAgent;
        assert_eq!(
            agent.sweep_strategy(),
            SweepStrategy::Periodic(Duration::from_secs(30 * 60))
        );
    }

    #[test]
    fn test_read_incremental_basic() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"user","message":{{"content":"Hello"}},"timestamp":"2025-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Hi there"}}],"model":"claude-sonnet-4"}},"timestamp":"2025-01-01T00:00:01Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = ClaudeAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test-session")
            .unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, Some("claude-sonnet-4".to_string()));
    }
}
