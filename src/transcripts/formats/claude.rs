//! Claude Code JSONL transcript reader.
//!
//! Reads Claude Code transcript files incrementally from a byte offset watermark.
//! Format: JSONL with entries like:
//! ```json
//! {"type": "user", "message": {"content": "Hello"}, "timestamp": "2025-01-01T00:00:00Z"}
//! {"type": "assistant", "message": {"content": [{"type": "text", "text": "Hi"}], "model": "claude-3"}, "timestamp": "..."}
//! {"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Read", "input": {...}, "id": "toolu_123"}]}}
//! ```

use crate::metrics::events::AgentTraceValues;
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

/// Read Claude Code transcript incrementally from watermark position.
///
/// # Arguments
///
/// * `path` - Path to the JSONL transcript file
/// * `watermark` - Byte offset to start reading from
/// * `session_id` - Session ID for this transcript (used for error context)
///
/// # Returns
///
/// `TranscriptBatch` with:
/// - `events`: Vector of `AgentTraceValues` for each message/tool use
/// - `model`: Model name if found in assistant messages
/// - `new_watermark`: Updated byte offset after processing
///
/// # Errors
///
/// - `Transient`: File locked or temporary I/O error
/// - `Parse`: Malformed JSON line at specific line number
/// - `Fatal`: File not found or permissions error
pub fn read_incremental(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_read_incremental_from_start() {
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

        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_incremental(file.path(), watermark, "test-session").unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, Some("claude-sonnet-4".to_string()));

        // Check first event (user message)
        let event0 = &result.events[0];
        assert_eq!(event0.event_type, Some(Some("user_message".to_string())));
        assert_eq!(event0.prompt_text, Some(Some("Hello".to_string())));

        // Check second event (assistant message)
        let event1 = &result.events[1];
        assert_eq!(
            event1.event_type,
            Some(Some("assistant_message".to_string()))
        );
        assert_eq!(event1.response_text, Some(Some("Hi there".to_string())));

        // Watermark should have advanced
        let new_offset = result
            .new_watermark
            .as_any()
            .downcast_ref::<ByteOffsetWatermark>()
            .unwrap()
            .0;
        assert!(new_offset > 0);
    }

    #[test]
    fn test_read_incremental_with_tool_use() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Read","id":"toolu_123"}}],"model":"claude-sonnet-4"}},"timestamp":"2025-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_incremental(file.path(), watermark, "test-session").unwrap();

        assert_eq!(result.events.len(), 1);
        assert_eq!(result.model, Some("claude-sonnet-4".to_string()));

        let event = &result.events[0];
        assert_eq!(event.event_type, Some(Some("tool_use".to_string())));
        assert_eq!(event.tool_name, Some(Some("Read".to_string())));
        assert_eq!(event.tool_use_id, Some(Some("toolu_123".to_string())));
    }

    #[test]
    fn test_read_incremental_resume_from_watermark() {
        let mut file = NamedTempFile::new().unwrap();
        let line1 =
            r#"{"type":"user","message":{"content":"First"},"timestamp":"2025-01-01T00:00:00Z"}"#;
        let line2 =
            r#"{"type":"user","message":{"content":"Second"},"timestamp":"2025-01-01T00:00:01Z"}"#;
        writeln!(file, "{}", line1).unwrap();
        writeln!(file, "{}", line2).unwrap();
        file.flush().unwrap();

        // First read from start
        let watermark1 = Box::new(ByteOffsetWatermark::new(0));
        let result1 = read_incremental(file.path(), watermark1, "test-session").unwrap();
        assert_eq!(result1.events.len(), 2);

        // Get watermark after first line
        let first_line_offset = (line1.len() + 1) as u64; // +1 for newline

        // Second read from watermark (should only get second line)
        let watermark2 = Box::new(ByteOffsetWatermark::new(first_line_offset));
        let result2 = read_incremental(file.path(), watermark2, "test-session").unwrap();
        assert_eq!(result2.events.len(), 1);
        assert_eq!(
            result2.events[0].prompt_text,
            Some(Some("Second".to_string()))
        );
    }

    #[test]
    fn test_read_incremental_empty_file() {
        let file = NamedTempFile::new().unwrap();
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_incremental(file.path(), watermark, "test-session").unwrap();

        assert_eq!(result.events.len(), 0);
        assert_eq!(result.model, None);
    }

    #[test]
    fn test_read_incremental_malformed_json() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "{{invalid json}}").unwrap();
        file.flush().unwrap();

        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_incremental(file.path(), watermark, "test-session");

        assert!(matches!(
            result,
            Err(TranscriptError::Parse { line: 1, .. })
        ));
    }

    #[test]
    fn test_read_incremental_file_not_found() {
        let path = Path::new("/nonexistent/path/to/transcript.jsonl");
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_incremental(path, watermark, "test-session");

        assert!(matches!(result, Err(TranscriptError::Fatal { .. })));
    }

    #[test]
    fn test_read_incremental_skips_empty_lines() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"user","message":{{"content":"First"}},"timestamp":"2025-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(file, "").unwrap(); // Empty line
        writeln!(
            file,
            r#"{{"type":"user","message":{{"content":"Second"}},"timestamp":"2025-01-01T00:00:01Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_incremental(file.path(), watermark, "test-session").unwrap();

        assert_eq!(result.events.len(), 2);
    }

    #[test]
    fn test_read_incremental_user_content_array() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"user","message":{{"content":[{{"type":"text","text":"Hello"}},{{"type":"text","text":"World"}}]}},"timestamp":"2025-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_incremental(file.path(), watermark, "test-session").unwrap();

        assert_eq!(result.events.len(), 1);
        assert_eq!(
            result.events[0].prompt_text,
            Some(Some("Hello\nWorld".to_string()))
        );
    }

    #[test]
    fn test_read_incremental_skips_tool_results_in_user_content() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"user","message":{{"content":[{{"type":"text","text":"Question"}},{{"type":"tool_result","content":"Result"}}]}},"timestamp":"2025-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_incremental(file.path(), watermark, "test-session").unwrap();

        assert_eq!(result.events.len(), 1);
        // Should only contain the text, not the tool_result
        assert_eq!(
            result.events[0].prompt_text,
            Some(Some("Question".to_string()))
        );
    }
}
