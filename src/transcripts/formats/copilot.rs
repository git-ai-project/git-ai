//! GitHub Copilot transcript reader.
//!
//! Supports two Copilot transcript formats:
//! 1. Session JSON: Single JSON file with complete session data
//! 2. Event stream JSONL: Append-only log of events

use crate::metrics::events::AgentTraceValues;
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

/// Read Copilot session JSON incrementally.
///
/// Session JSON format is a single JSON file with a "requests" array.
/// Uses byte offset watermark for incremental reading (though in practice
/// the file is typically read whole).
///
/// # Arguments
///
/// * `path` - Path to the session JSON file
/// * `watermark` - Byte offset to start reading from
/// * `session_id` - Session ID for this transcript
///
/// # Returns
///
/// `TranscriptBatch` with parsed events and optional model info.
///
/// # Errors
///
/// - `Transient`: File locked or temporary I/O error
/// - `Parse`: Malformed JSON
/// - `Fatal`: File not found, permissions error, or running in Codespaces/Remote Containers
pub fn read_session_json(
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
                "Copilot session reader requires ByteOffsetWatermark, got incompatible type for session {}",
                session_id
            ),
        })?;

    // Check if running in Codespaces or Remote Containers - if so, return empty transcript
    let is_codespaces = std::env::var("CODESPACES").ok().as_deref() == Some("true");
    let is_remote_containers = std::env::var("REMOTE_CONTAINERS").ok().as_deref() == Some("true");

    if is_codespaces || is_remote_containers {
        return Ok(TranscriptBatch {
            events: Vec::new(),
            model: None,
            new_watermark: watermark,
        });
    }

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
                retry_after: std::time::Duration::from_secs(5),
            }
        }
    })?;

    // If we already read this content (watermark at end), return empty batch
    if byte_watermark.0 >= content.len() as u64 {
        return Ok(TranscriptBatch {
            events: Vec::new(),
            model: None,
            new_watermark: watermark,
        });
    }

    // Parse the JSON
    let session_json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| TranscriptError::Parse {
            line: 0,
            message: format!("Invalid JSON in {}: {}", path.display(), e),
        })?;

    // Check if this looks like an event stream format (should use read_event_stream instead)
    if looks_like_event_stream(&session_json) {
        return read_event_stream(path, Box::new(ByteOffsetWatermark::new(0)), session_id);
    }

    // Extract the requests array
    let requests = session_json
        .get("requests")
        .and_then(|v| v.as_array())
        .ok_or_else(|| TranscriptError::Parse {
            line: 0,
            message: "requests array not found in Copilot session JSON".to_string(),
        })?;

    // Extract session-level model
    let model = session_json
        .get("inputState")
        .and_then(|is| is.get("selectedModel"))
        .and_then(|sm| sm.get("identifier"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut events = Vec::new();

    for request in requests {
        // Parse the user timestamp
        let user_ts_opt = request
            .get("timestamp")
            .and_then(|v| v.as_i64())
            .and_then(|ms| {
                chrono::TimeZone::timestamp_millis_opt(&chrono::Utc, ms)
                    .single()
                    .map(|dt| dt.timestamp() as u64)
            });

        // Add the user's message
        if let Some(user_text) = request
            .get("message")
            .and_then(|m| m.get("text"))
            .and_then(|v| v.as_str())
        {
            let trimmed = user_text.trim();
            if !trimmed.is_empty() {
                let event = AgentTraceValues::new()
                    .event_type("user_message")
                    .prompt_text(trimmed);

                let event = if let Some(ts) = user_ts_opt {
                    event.event_ts(ts)
                } else {
                    event
                };

                events.push(event);
            }
        }

        // Process assistant response items
        if let Some(response_items) = request.get("response").and_then(|v| v.as_array()) {
            for item in response_items {
                // Handle different kinds of response items
                if let Some(kind) = item.get("kind").and_then(|v| v.as_str()) {
                    match kind {
                        "markdownContent" => {
                            if let Some(text) = item.get("value").and_then(|v| v.as_str())
                                && !text.trim().is_empty()
                            {
                                let event = AgentTraceValues::new()
                                    .event_type("assistant_message")
                                    .response_text(text);

                                let event = if let Some(ts) = user_ts_opt {
                                    event.event_ts(ts)
                                } else {
                                    event
                                };

                                events.push(event);
                            }
                        }
                        "toolInvocationSerialized" => {
                            if let Some(tool_name) = item.get("toolId").and_then(|v| v.as_str()) {
                                let mut event = AgentTraceValues::new()
                                    .event_type("tool_use")
                                    .tool_name(tool_name);

                                if let Some(ts) = user_ts_opt {
                                    event = event.event_ts(ts);
                                }

                                events.push(event);
                            }
                        }
                        "textEditGroup" | "prepareToolInvocation" => {
                            let mut event = AgentTraceValues::new()
                                .event_type("tool_use")
                                .tool_name(kind);

                            if let Some(ts) = user_ts_opt {
                                event = event.event_ts(ts);
                            }

                            events.push(event);
                        }
                        _ => {} // Skip other kinds
                    }
                }
            }
        }
    }

    // Update watermark to end of file
    let new_watermark = Box::new(ByteOffsetWatermark::new(content.len() as u64));

    Ok(TranscriptBatch {
        events,
        model,
        new_watermark,
    })
}

/// Read Copilot event stream JSONL incrementally.
///
/// Event stream format is JSONL with events like:
/// ```json
/// {"type": "session.start", "data": {...}, "timestamp": "2025-01-01T00:00:00Z"}
/// {"type": "user.message", "data": {"content": "Hello"}, "timestamp": "2025-01-01T00:00:01Z"}
/// {"type": "assistant.message", "data": {"content": "Hi", "toolRequests": [...]}, "timestamp": "2025-01-01T00:00:02Z"}
/// ```
///
/// # Arguments
///
/// * `path` - Path to the JSONL event stream file
/// * `watermark` - Byte offset to start reading from
/// * `session_id` - Session ID for this transcript
///
/// # Returns
///
/// `TranscriptBatch` with parsed events and optional model info.
///
/// # Errors
///
/// - `Transient`: File locked or temporary I/O error
/// - `Parse`: Malformed JSON line at specific line number
/// - `Fatal`: File not found or permissions error
pub fn read_event_stream(
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
                "Copilot event stream reader requires ByteOffsetWatermark, got incompatible type for session {}",
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

        // Update offset before processing
        current_offset += bytes_read as u64;

        // Skip empty lines
        if line.trim().is_empty() {
            continue;
        }

        // Parse JSONL entry
        let event: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| TranscriptError::Parse {
                line: line_number,
                message: format!("Invalid JSON in {}: {}", path.display(), e),
            })?;

        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let data = event.get("data");

        // Extract timestamp
        let timestamp_opt = event
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.timestamp() as u64)
            });

        // Try to extract model if we haven't found it yet
        if model.is_none()
            && let Some(d) = data
        {
            model = extract_model_hint(d);
        }

        // Process events based on type
        match event_type {
            "user.message" => {
                if let Some(text) = data
                    .and_then(|d| d.get("content"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
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
            "assistant.message" => {
                // Extract visible content or reasoning text
                let assistant_text = data
                    .and_then(|d| d.get("content"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        data.and_then(|d| d.get("reasoningText"))
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                    });

                if let Some(text) = assistant_text {
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

                // Extract tool requests
                if let Some(tool_requests) = data
                    .and_then(|d| d.get("toolRequests"))
                    .and_then(|v| v.as_array())
                {
                    for request in tool_requests {
                        let name = request
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool")
                            .to_string();

                        let mut event = AgentTraceValues::new()
                            .event_type("tool_use")
                            .tool_name(&name);

                        if let Some(ts) = timestamp_opt {
                            event = event.event_ts(ts);
                        }

                        events.push(event);
                    }
                }
            }
            "tool.execution_start" => {
                let name = data
                    .and_then(|d| d.get("toolName"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string();

                let mut event = AgentTraceValues::new()
                    .event_type("tool_use")
                    .tool_name(&name);

                if let Some(ts) = timestamp_opt {
                    event = event.event_ts(ts);
                }

                events.push(event);
            }
            _ => {} // Skip other event types
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

/// Check if a parsed JSON looks like a Copilot event stream format.
fn looks_like_event_stream(parsed: &serde_json::Value) -> bool {
    parsed
        .get("type")
        .and_then(|v| v.as_str())
        .map(|event_type| {
            parsed.get("data").map(|v| v.is_object()).unwrap_or(false)
                && parsed.get("kind").is_none()
                && (event_type.starts_with("session.")
                    || event_type.starts_with("assistant.")
                    || event_type.starts_with("user.")
                    || event_type.starts_with("tool."))
        })
        .unwrap_or(false)
}

/// Extract model hint from Copilot data.
fn extract_model_hint(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            // Check for direct model fields
            if let Some(model_id) = map.get("modelId").and_then(|v| v.as_str())
                && model_id.starts_with("copilot/")
            {
                return Some(model_id.to_string());
            }
            if let Some(model) = map.get("model").and_then(|v| v.as_str())
                && model.starts_with("copilot/")
            {
                return Some(model.to_string());
            }
            if let Some(identifier) = map
                .get("selectedModel")
                .and_then(|v| v.get("identifier"))
                .and_then(|v| v.as_str())
                && identifier.starts_with("copilot/")
            {
                return Some(identifier.to_string());
            }
            // Recursively search nested objects
            for val in map.values() {
                if let Some(found) = extract_model_hint(val) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(extract_model_hint),
        serde_json::Value::String(s) => {
            if s.starts_with("copilot/") {
                Some(s.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_read_session_json_basic() {
        let mut file = NamedTempFile::new().unwrap();
        let json = r#"{
            "requests": [
                {
                    "timestamp": 1704067200000,
                    "message": {"text": "Hello"},
                    "response": [
                        {"kind": "markdownContent", "value": "Hi there"}
                    ]
                }
            ],
            "inputState": {
                "selectedModel": {"identifier": "copilot/gpt-4"}
            }
        }"#;
        write!(file, "{}", json).unwrap();
        file.flush().unwrap();

        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_session_json(file.path(), watermark, "test-session").unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, Some("copilot/gpt-4".to_string()));

        // Check user message
        assert_eq!(
            result.events[0].event_type,
            Some(Some("user_message".to_string()))
        );
        assert_eq!(
            result.events[0].prompt_text,
            Some(Some("Hello".to_string()))
        );

        // Check assistant message
        assert_eq!(
            result.events[1].event_type,
            Some(Some("assistant_message".to_string()))
        );
        assert_eq!(
            result.events[1].response_text,
            Some(Some("Hi there".to_string()))
        );
    }

    #[test]
    fn test_read_event_stream_basic() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"user.message","data":{{"content":"Hello"}},"timestamp":"2025-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant.message","data":{{"content":"Hi there","modelId":"copilot/gpt-4"}},"timestamp":"2025-01-01T00:00:01Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_event_stream(file.path(), watermark, "test-session").unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, Some("copilot/gpt-4".to_string()));

        // Check user message
        assert_eq!(
            result.events[0].event_type,
            Some(Some("user_message".to_string()))
        );
        assert_eq!(
            result.events[0].prompt_text,
            Some(Some("Hello".to_string()))
        );

        // Check assistant message
        assert_eq!(
            result.events[1].event_type,
            Some(Some("assistant_message".to_string()))
        );
        assert_eq!(
            result.events[1].response_text,
            Some(Some("Hi there".to_string()))
        );
    }

    #[test]
    fn test_read_event_stream_with_tool_requests() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant.message","data":{{"content":"Let me read that","toolRequests":[{{"name":"read_file"}}]}},"timestamp":"2025-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = read_event_stream(file.path(), watermark, "test-session").unwrap();

        assert_eq!(result.events.len(), 2); // One assistant message + one tool use

        // Check tool use
        assert_eq!(
            result.events[1].event_type,
            Some(Some("tool_use".to_string()))
        );
        assert_eq!(
            result.events[1].tool_name,
            Some(Some("read_file".to_string()))
        );
    }

    #[test]
    fn test_read_event_stream_resume_from_watermark() {
        let mut file = NamedTempFile::new().unwrap();
        let line1 = r#"{"type":"user.message","data":{"content":"First"},"timestamp":"2025-01-01T00:00:00Z"}"#;
        let line2 = r#"{"type":"user.message","data":{"content":"Second"},"timestamp":"2025-01-01T00:00:01Z"}"#;
        writeln!(file, "{}", line1).unwrap();
        writeln!(file, "{}", line2).unwrap();
        file.flush().unwrap();

        // Get watermark after first line
        let first_line_offset = (line1.len() + 1) as u64; // +1 for newline

        // Read from watermark (should only get second line)
        let watermark = Box::new(ByteOffsetWatermark::new(first_line_offset));
        let result = read_event_stream(file.path(), watermark, "test-session").unwrap();
        assert_eq!(result.events.len(), 1);
        assert_eq!(
            result.events[0].prompt_text,
            Some(Some("Second".to_string()))
        );
    }

    #[test]
    fn test_looks_like_event_stream() {
        let event_stream = serde_json::json!({
            "type": "user.message",
            "data": {"content": "test"}
        });
        assert!(looks_like_event_stream(&event_stream));

        let session_json = serde_json::json!({
            "requests": []
        });
        assert!(!looks_like_event_stream(&session_json));

        let jsonl_wrapper = serde_json::json!({
            "kind": 0,
            "v": {}
        });
        assert!(!looks_like_event_stream(&jsonl_wrapper));
    }

    #[test]
    fn test_extract_model_hint() {
        let data = serde_json::json!({"modelId": "copilot/gpt-4"});
        assert_eq!(extract_model_hint(&data), Some("copilot/gpt-4".to_string()));

        let data = serde_json::json!({"model": "copilot/claude"});
        assert_eq!(
            extract_model_hint(&data),
            Some("copilot/claude".to_string())
        );

        let data = serde_json::json!({"selectedModel": {"identifier": "copilot/sonnet"}});
        assert_eq!(
            extract_model_hint(&data),
            Some("copilot/sonnet".to_string())
        );

        let data = serde_json::json!({"foo": "bar"});
        assert_eq!(extract_model_hint(&data), None);
    }
}
