//! Windsurf agent implementation with sweep discovery.

use crate::metrics::events::AgentTraceValues;
use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

/// Windsurf agent that reads Windsurf JSONL transcript files.
pub struct WindsurfAgent;

impl Agent for WindsurfAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        // Sweep not fully implemented for Windsurf yet — discovery comes from presets
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
                    "Windsurf reader requires ByteOffsetWatermark, got incompatible type for session {}",
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
        let mut current_offset = start_offset;
        let mut line_number = 0;

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

            // Extract timestamp if available
            let timestamp_opt = entry["timestamp"].as_str().and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.timestamp() as u64)
            });

            // Get entry type and optional inner object matching the type name
            let entry_type = match entry["type"].as_str() {
                Some(t) => t,
                None => continue,
            };
            let inner = entry.get(entry_type);

            // Parse by entry type
            match entry_type {
                "user_input" => {
                    if let Some(text) = inner.and_then(|obj| obj["user_response"].as_str())
                        && !text.trim().is_empty()
                    {
                        let mut event = AgentTraceValues::new()
                            .event_type("user_message")
                            .prompt_text(text);

                        if let Some(ts) = timestamp_opt {
                            event = event.event_ts(ts);
                        }

                        events.push(event);
                    }
                }
                "planner_response" => {
                    if let Some(text) = inner.and_then(|obj| obj["response"].as_str())
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
                "code_action" => {
                    let mut event = AgentTraceValues::new()
                        .event_type("tool_use")
                        .tool_name("code_action");

                    if let Some(ts) = timestamp_opt {
                        event = event.event_ts(ts);
                    }

                    events.push(event);
                }
                "view_file" | "run_command" | "find" | "grep_search" | "list_directory"
                | "list_resources" => {
                    let mut event = AgentTraceValues::new()
                        .event_type("tool_use")
                        .tool_name(entry_type);

                    if let Some(ts) = timestamp_opt {
                        event = event.event_ts(ts);
                    }

                    events.push(event);
                }
                _ => {} // Skip all other types
            }
        }

        // Create new watermark with updated offset
        let new_watermark = Box::new(ByteOffsetWatermark::new(current_offset));

        // Model is not available in Windsurf JSONL format
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
        let agent = WindsurfAgent;
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
            r#"{{"type":"user_input","user_input":{{"user_response":"Hello"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"planner_response","planner_response":{{"response":"Hi there"}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = WindsurfAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, None);
    }

    #[test]
    fn test_read_incremental_tool_actions() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"code_action","code_action":{{"path":"test.rs","new_content":"fn main()"}}}}"#).unwrap();
        writeln!(
            file,
            r#"{{"type":"run_command","run_command":{{"command":"cargo test"}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = WindsurfAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 2);
    }

    #[test]
    fn test_read_incremental_resumes_from_offset() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        let line1 = r#"{"type":"user_input","user_input":{"user_response":"First"}}"#;
        let line2 = r#"{"type":"user_input","user_input":{"user_response":"Second"}}"#;
        writeln!(file, "{}", line1).unwrap();
        writeln!(file, "{}", line2).unwrap();
        file.flush().unwrap();

        let agent = WindsurfAgent;

        // First read gets both
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();
        assert_eq!(result.events.len(), 2);

        // Second read from new watermark gets nothing
        let result2 = agent
            .read_incremental(file.path(), result.new_watermark, "test")
            .unwrap();
        assert_eq!(result2.events.len(), 0);
    }
}
