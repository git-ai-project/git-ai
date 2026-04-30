//! GitHub Copilot agent implementation with sweep discovery.

use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy, TranscriptFormat};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy, WatermarkType};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// GitHub Copilot agent that discovers conversations from Copilot storage.
pub struct CopilotAgent;

impl CopilotAgent {
    /// Scan for Copilot transcript files in standard locations.
    ///
    /// Discovers BOTH session.json files and .jsonl event streams.
    fn scan_transcript_files() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Standard locations for Copilot transcripts
        let search_dirs = vec![
            // Session JSON files
            dirs::config_dir().map(|p| p.join("github-copilot/sessions")),
            // Event stream JSONL files
            dirs::config_dir().map(|p| p.join("github-copilot/events")),
        ];

        for dir_opt in search_dirs {
            if let Some(dir) = dir_opt
                && dir.exists()
                && let Ok(entries) = fs::read_dir(&dir)
            {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        let ext = path.extension().and_then(|s| s.to_str());
                        // Accept both .json (session files) and .jsonl (event streams)
                        if ext == Some("json") || ext == Some("jsonl") {
                            paths.push(path);
                        }
                    }
                }
            }
        }

        paths
    }

    /// Extract session ID from a Copilot transcript file path.
    ///
    /// Copilot files are typically named like: `<uuid>.json` or `<uuid>.jsonl`
    fn extract_session_id(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| format!("copilot:{}", s))
    }

    /// Determine transcript format from file path.
    fn determine_format(path: &Path) -> TranscriptFormat {
        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            TranscriptFormat::CopilotEventStreamJsonl
        } else {
            TranscriptFormat::CopilotSessionJson
        }
    }

}

impl Agent for CopilotAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        // Poll every 30 minutes for new Copilot transcripts
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        let paths = Self::scan_transcript_files();
        let mut sessions = Vec::new();

        for path in paths {
            let Some(session_id) = Self::extract_session_id(&path) else {
                continue;
            };

            // Determine format from file extension (no I/O, just checking path)
            let format = Self::determine_format(&path);

            // Don't parse file content here - just filesystem scanning.
            // Model will be extracted later during first read_incremental() if needed.
            let session = DiscoveredSession {
                session_id,
                agent_type: "copilot".to_string(),
                transcript_path: path,
                transcript_format: format,
                watermark_type: WatermarkType::ByteOffset,
                initial_watermark: Box::new(ByteOffsetWatermark::new(0)),
                model: None,
                tool: Some("GitHub Copilot".to_string()),
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
        // Migrated from formats/copilot.rs (will be removed in Phase 9)
        // Determine which reader to use based on file extension
        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            read_event_stream(path, watermark, session_id)
        } else {
            read_session_json(path, watermark, session_id)
        }
    }
}

/// Read Copilot session JSON incrementally.
fn read_session_json(
    path: &Path,
    watermark: Box<dyn WatermarkStrategy>,
    session_id: &str,
) -> Result<TranscriptBatch, TranscriptError> {
    use crate::metrics::events::AgentTraceValues;

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
fn read_event_stream(
    path: &Path,
    watermark: Box<dyn WatermarkStrategy>,
    session_id: &str,
) -> Result<TranscriptBatch, TranscriptError> {
    use crate::metrics::events::AgentTraceValues;
    use std::fs::File;
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

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

    #[test]
    fn test_extract_session_id() {
        let path = PathBuf::from("/home/user/.config/github-copilot/sessions/abc-123.json");
        let session_id = CopilotAgent::extract_session_id(&path);
        assert_eq!(session_id, Some("copilot:abc-123".to_string()));
    }

    #[test]
    fn test_sweep_strategy() {
        let agent = CopilotAgent;
        assert_eq!(
            agent.sweep_strategy(),
            SweepStrategy::Periodic(Duration::from_secs(30 * 60))
        );
    }

    #[test]
    fn test_determine_format() {
        let json_path = PathBuf::from("/path/to/session.json");
        assert_eq!(
            CopilotAgent::determine_format(&json_path),
            TranscriptFormat::CopilotSessionJson
        );

        let jsonl_path = PathBuf::from("/path/to/events.jsonl");
        assert_eq!(
            CopilotAgent::determine_format(&jsonl_path),
            TranscriptFormat::CopilotEventStreamJsonl
        );
    }

    #[test]
    fn test_read_session_json_basic() {
        use std::io::Write;
        use tempfile::NamedTempFile;

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

        let agent = CopilotAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test-session")
            .unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, Some("copilot/gpt-4".to_string()));
    }

    #[test]
    fn test_read_event_stream_basic() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create a .jsonl file
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
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

        let agent = CopilotAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test-session")
            .unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, Some("copilot/gpt-4".to_string()));
    }
}
