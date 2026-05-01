//! Cursor agent implementation with sweep discovery.

use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy, TranscriptFormat};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy, WatermarkType};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Cursor agent that discovers conversations from Cursor storage.
pub struct CursorAgent;

impl CursorAgent {
    /// Scan for Cursor conversation files in standard locations.
    fn scan_conversation_files() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Standard location for Cursor transcripts
        let search_dirs =
            vec![dirs::config_dir().map(|p| p.join("Cursor/User/globalStorage/conversations"))];

        for dir_opt in search_dirs {
            if let Some(dir) = dir_opt
                && dir.exists()
                && let Ok(entries) = fs::read_dir(&dir)
            {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().map(|ext| ext == "jsonl").unwrap_or(false)
                    {
                        paths.push(path);
                    }
                }
            }
        }

        paths
    }

    /// Extract session ID from a Cursor conversation file path.
    ///
    /// Cursor files are typically named like: `<uuid>.jsonl`
    fn extract_session_id(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| format!("cursor:{}", s))
    }
}

impl Agent for CursorAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        // Poll every 30 minutes for new Cursor conversations
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        let paths = Self::scan_conversation_files();
        let mut sessions = Vec::new();

        for path in paths {
            let Some(session_id) = Self::extract_session_id(&path) else {
                continue;
            };

            // Don't parse file content here - just filesystem scanning.
            // Model will be extracted later during first read_incremental() if needed.
            let session = DiscoveredSession {
                session_id,
                agent_type: "cursor".to_string(),
                transcript_path: path,
                transcript_format: TranscriptFormat::CursorJsonl,
                watermark_type: WatermarkType::ByteOffset,
                initial_watermark: Box::new(ByteOffsetWatermark::new(0)),
                model: None,
                tool: Some("Cursor".to_string()),
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
        // Migrated from formats/cursor.rs (will be removed in Phase 9)
        use crate::metrics::events::AgentTraceValues;
        use std::fs::File;
        use std::io::{BufRead, BufReader, Seek, SeekFrom};

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
            let bytes_read =
                reader
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

    #[test]
    fn test_extract_session_id() {
        let path = PathBuf::from(
            "/home/user/.config/Cursor/User/globalStorage/conversations/abc-123.jsonl",
        );
        let session_id = CursorAgent::extract_session_id(&path);
        assert_eq!(session_id, Some("cursor:abc-123".to_string()));
    }

    #[test]
    fn test_sweep_strategy() {
        let agent = CursorAgent;
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
            r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"Hello"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"Hi there"}}]}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = CursorAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test-session")
            .unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, None); // Cursor doesn't have model in JSONL
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
