//! Pi agent implementation with sweep discovery.

use crate::metrics::events::AgentTraceValues;
use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy};
use chrono::DateTime;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

/// Pi agent that reads Pi JSONL session files.
pub struct PiAgent;

impl Agent for PiAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        // Discovery happens via presets, not filesystem scanning
        Ok(Vec::new())
    }

    fn read_incremental(
        &self,
        path: &Path,
        watermark: Box<dyn WatermarkStrategy>,
        session_id: &str,
    ) -> Result<TranscriptBatch, TranscriptError> {
        // Downcast watermark to ByteOffsetWatermark
        let byte_watermark = watermark
            .as_any()
            .downcast_ref::<ByteOffsetWatermark>()
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "Pi reader requires ByteOffsetWatermark, got incompatible type for session {}",
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
                    retry_after: Duration::from_secs(5),
                }
            }
        })?;

        let mut reader = BufReader::new(file);

        // Seek to watermark position
        reader
            .seek(SeekFrom::Start(start_offset))
            .map_err(|e| TranscriptError::Transient {
                message: format!("Failed to seek to offset {}: {}", start_offset, e),
                retry_after: Duration::from_secs(5),
            })?;

        let mut events = Vec::new();
        let mut latest_model: Option<String> = None;
        let mut current_offset = start_offset;
        let mut line_number = 0;
        let mut saw_session_header = false;

        // Read lines from watermark position
        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read =
                reader
                    .read_line(&mut line)
                    .map_err(|e| TranscriptError::Transient {
                        message: format!("I/O error reading line: {}", e),
                        retry_after: Duration::from_secs(5),
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

            let entry_type = entry["type"].as_str().unwrap_or("");

            match entry_type {
                "session" => {
                    saw_session_header = true;
                }
                "message" => {
                    // Extract message object (Fatal if missing)
                    let message = entry.get("message").ok_or_else(|| TranscriptError::Fatal {
                        message: format!(
                            "Pi session file entry missing 'message' object at line {} in {}",
                            line_number,
                            path.display()
                        ),
                    })?;

                    // Extract role (Fatal if missing)
                    let role = message["role"]
                        .as_str()
                        .ok_or_else(|| TranscriptError::Fatal {
                            message: format!(
                                "Pi session file message missing 'role' at line {} in {}",
                                line_number,
                                path.display()
                            ),
                        })?;

                    // Extract timestamp: try message.timestamp as i64 (milliseconds) first,
                    // then fall back to entry.timestamp as RFC3339 string
                    let timestamp_opt: Option<u64> = message["timestamp"]
                        .as_i64()
                        .and_then(|ms| {
                            DateTime::from_timestamp_millis(ms).map(|dt| dt.timestamp() as u64)
                        })
                        .or_else(|| {
                            entry["timestamp"].as_str().and_then(|s| {
                                DateTime::parse_from_rfc3339(s)
                                    .ok()
                                    .map(|dt| dt.timestamp() as u64)
                            })
                        });

                    match role {
                        "user" => {
                            // Content can be String (direct text) or Array (iterate blocks)
                            let text = if let Some(s) = message["content"].as_str() {
                                s.to_string()
                            } else if let Some(content_array) = message["content"].as_array() {
                                let mut texts = Vec::new();
                                for block in content_array {
                                    if block["type"].as_str() == Some("text")
                                        && let Some(text) = block["text"].as_str()
                                    {
                                        texts.push(text.to_string());
                                    }
                                }
                                texts.join("\n")
                            } else {
                                String::new()
                            };

                            if !text.trim().is_empty() {
                                let mut event = AgentTraceValues::new()
                                    .event_type("user_message")
                                    .prompt_text(text);

                                if let Some(ts) = timestamp_opt {
                                    event = event.event_ts(ts);
                                }

                                events.push(event);
                            }
                        }
                        "assistant" => {
                            // Extract model from message.model (update latest_model if non-empty)
                            if let Some(model_str) = message["model"].as_str()
                                && !model_str.is_empty()
                            {
                                latest_model = Some(model_str.to_string());
                            }

                            // Content is Array of blocks
                            if let Some(content_array) = message["content"].as_array() {
                                for block in content_array {
                                    match block["type"].as_str() {
                                        Some("text") => {
                                            if let Some(text) = block["text"].as_str()
                                                && !text.trim().is_empty()
                                            {
                                                let mut event = AgentTraceValues::new()
                                                    .event_type("assistant_message")
                                                    .response_text(text);

                                                if let Some(ts) = timestamp_opt {
                                                    event = event.event_ts(ts);
                                                }

                                                events.push(event);
                                            }
                                        }
                                        Some("thinking") => {
                                            if let Some(thinking) = block["thinking"].as_str()
                                                && !thinking.trim().is_empty()
                                            {
                                                let mut event = AgentTraceValues::new()
                                                    .event_type("assistant_thinking")
                                                    .response_text(thinking);

                                                if let Some(ts) = timestamp_opt {
                                                    event = event.event_ts(ts);
                                                }

                                                events.push(event);
                                            }
                                        }
                                        Some("toolCall") => {
                                            if let Some(name) = block["name"].as_str() {
                                                let mut event = AgentTraceValues::new()
                                                    .event_type("tool_use")
                                                    .tool_name(name);

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
                        "toolResult" => {
                            // Tool results become assistant messages in the transcript
                            if let Some(content_array) = message["content"].as_array() {
                                for block in content_array {
                                    if block["type"].as_str() == Some("text")
                                        && let Some(text) = block["text"].as_str()
                                        && !text.trim().is_empty()
                                    {
                                        let mut event = AgentTraceValues::new()
                                            .event_type("assistant_message")
                                            .response_text(text);

                                        if let Some(ts) = timestamp_opt {
                                            event = event.event_ts(ts);
                                        }

                                        events.push(event);
                                    }
                                }
                            }
                        }
                        // Skip custom, branchSummary, compactionSummary, and unknown roles
                        _ => {}
                    }
                }
                // Skip all other entry types: custom, branch_summary, branchSummary, compaction,
                // compactionSummary, model_change, thinking_level_change, label, session_info,
                // custom_message, and unknown types
                _ => {}
            }
        }

        // If this is the very first read (offset==0) and we never saw a session header, return Fatal
        if start_offset == 0 && !saw_session_header {
            return Err(TranscriptError::Fatal {
                message: format!(
                    "Pi session file is missing a session header: {}",
                    path.display()
                ),
            });
        }

        // Create new watermark with updated offset
        let new_watermark = Box::new(ByteOffsetWatermark::new(current_offset));

        Ok(TranscriptBatch {
            events,
            model: latest_model,
            new_watermark,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sweep_strategy() {
        let agent = PiAgent;
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
        writeln!(file, r#"{{"type":"session","id":"s1"}}"#).unwrap();
        writeln!(
            file,
            r#"{{"type":"message","message":{{"role":"user","content":"Hello","timestamp":1704067200000}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"message","message":{{"role":"assistant","content":[{{"type":"text","text":"Hi"}}],"model":"claude-sonnet-4-20250514"}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = PiAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, Some("claude-sonnet-4-20250514".to_string()));
    }

    #[test]
    fn test_read_incremental_missing_session_header() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"message","message":{{"role":"user","content":"Hello"}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = PiAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent.read_incremental(file.path(), watermark, "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_read_incremental_resume_no_header_needed() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // When resuming from non-zero offset, session header is not required.
        // Write a first line (which we'll skip via offset) and a second message line.
        let mut file = NamedTempFile::new().unwrap();
        let first_line = r#"{"type":"session","id":"s1"}"#;
        writeln!(file, "{}", first_line).unwrap();
        writeln!(
            file,
            r#"{{"type":"message","message":{{"role":"user","content":"Hello"}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = PiAgent;
        // Set offset past the first line (first_line bytes + newline) to simulate resuming
        let offset = (first_line.len() + 1) as u64;
        let watermark = Box::new(ByteOffsetWatermark::new(offset));
        let result = agent.read_incremental(file.path(), watermark, "test");
        assert!(result.is_ok());
    }

    #[test]
    fn test_read_incremental_thinking_and_tool_call() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"session","id":"s1"}}"#).unwrap();
        writeln!(
            file,
            r#"{{"type":"message","message":{{"role":"assistant","content":[{{"type":"thinking","thinking":"hmm"}},{{"type":"toolCall","name":"bash","arguments":{{}}}}]}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = PiAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 2); // thinking + toolCall
    }
}
