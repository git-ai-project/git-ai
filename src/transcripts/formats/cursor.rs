//! Cursor JSONL transcript reader.
//!
//! Reads Cursor transcript files incrementally from a byte offset watermark.
//! Format: JSONL with entries like:
//! ```json
//! {"role": "user", "message": {"content": [{"type": "text", "text": "Hello"}]}}
//! {"role": "assistant", "message": {"content": [{"type": "text", "text": "Hi"}]}}
//! {"role": "assistant", "message": {"content": [{"type": "tool_use", "name": "Read", "input": {...}}]}}
//! ```

use crate::metrics::events::AgentTraceValues;
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

/// Read Cursor transcript incrementally from watermark position.
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
/// - `model`: Model name (Cursor doesn't store this in JSONL, so None)
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
                "Cursor reader requires ByteOffsetWatermark, got incompatible type for session {}",
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

        // Cursor doesn't have timestamps in the JSONL format
        let timestamp_opt = None;

        // Extract events based on role
        match entry["role"].as_str() {
            Some("user") => {
                // User message - extract text content from content array
                if let Some(content_array) = entry["message"]["content"].as_array() {
                    let mut texts = Vec::new();
                    for item in content_array {
                        // Skip tool_result items - those are system-generated responses
                        if item["type"].as_str() == Some("tool_result") {
                            continue;
                        }
                        if item["type"].as_str() == Some("text")
                            && let Some(text) = item["text"].as_str()
                        {
                            // Strip Cursor's <user_query>...</user_query> wrapper tags
                            let cleaned = strip_cursor_user_query_tags(text);
                            if !cleaned.is_empty() {
                                texts.push(cleaned);
                            }
                        }
                    }

                    if !texts.is_empty() {
                        let event = AgentTraceValues::new()
                            .event_type("user_message")
                            .prompt_text(texts.join("\n"));

                        events.push(event);
                    }
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

                                    events.push(event);
                                }
                            }
                            Some("tool_use") => {
                                if let Some(name) = item["name"].as_str() {
                                    let mut event = AgentTraceValues::new()
                                        .event_type("tool_use")
                                        .tool_name(name);

                                    // Cursor doesn't typically have tool_use IDs in the same format
                                    if let Some(id) = item["id"].as_str() {
                                        event = event.external_tool_use_id(id);
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
            _ => {} // Skip unknown roles
        }
    }

    // Create new watermark with updated offset
    let new_watermark = Box::new(ByteOffsetWatermark::new(current_offset));

    // Cursor doesn't store model in JSONL - it comes from hook input
    Ok(TranscriptBatch {
        events,
        model: None,
        new_watermark,
    })
}

/// Strip `<user_query>...</user_query>` wrapper tags from Cursor user messages.
fn strip_cursor_user_query_tags(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(inner) = trimmed
        .strip_prefix("<user_query>")
        .and_then(|s| s.strip_suffix("</user_query>"))
    {
        inner.trim().to_string()
    } else {
        trimmed.to_string()
    }
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
            r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"Hello"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"Hi there"}}]}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_incremental(file.path(), watermark, "test-session").unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, None); // Cursor doesn't have model in JSONL

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
            r#"{{"role":"assistant","message":{{"content":[{{"type":"tool_use","name":"Read","id":"tool_123"}}]}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_incremental(file.path(), watermark, "test-session").unwrap();

        assert_eq!(result.events.len(), 1);

        let event = &result.events[0];
        assert_eq!(event.event_type, Some(Some("tool_use".to_string())));
        assert_eq!(event.tool_name, Some(Some("Read".to_string())));
        assert_eq!(event.external_tool_use_id, Some(Some("tool_123".to_string())));
    }

    #[test]
    fn test_read_incremental_strips_user_query_tags() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"<user_query>What is this?</user_query>"}}]}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_incremental(file.path(), watermark, "test-session").unwrap();

        assert_eq!(result.events.len(), 1);
        assert_eq!(
            result.events[0].prompt_text,
            Some(Some("What is this?".to_string()))
        );
    }

    #[test]
    fn test_read_incremental_skips_tool_results() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"Question"}},{{"type":"tool_result","content":"Result"}}]}}}}"#
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

    #[test]
    fn test_read_incremental_resume_from_watermark() {
        let mut file = NamedTempFile::new().unwrap();
        let line1 = r#"{"role":"user","message":{"content":[{"type":"text","text":"First"}]}}"#;
        let line2 = r#"{"role":"user","message":{"content":[{"type":"text","text":"Second"}]}}"#;
        writeln!(file, "{}", line1).unwrap();
        writeln!(file, "{}", line2).unwrap();
        file.flush().unwrap();

        // Get watermark after first line
        let first_line_offset = (line1.len() + 1) as u64; // +1 for newline

        // Read from watermark (should only get second line)
        let watermark = Box::new(ByteOffsetWatermark::new(first_line_offset));
        let result = read_incremental(file.path(), watermark, "test-session").unwrap();
        assert_eq!(result.events.len(), 1);
        assert_eq!(
            result.events[0].prompt_text,
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
    fn test_strip_cursor_user_query_tags() {
        assert_eq!(
            strip_cursor_user_query_tags("<user_query>Hello</user_query>"),
            "Hello"
        );
        assert_eq!(
            strip_cursor_user_query_tags("  <user_query>  Test  </user_query>  "),
            "Test"
        );
        assert_eq!(strip_cursor_user_query_tags("No tags here"), "No tags here");
        assert_eq!(
            strip_cursor_user_query_tags("<user_query>Partial"),
            "<user_query>Partial"
        );
    }
}
