//! Droid agent implementation with sweep discovery.

use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy, TranscriptFormat};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{HybridWatermark, WatermarkStrategy, WatermarkType};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Droid agent that discovers conversations from Droid storage.
pub struct DroidAgent;

impl DroidAgent {
    /// Scan for Droid conversation files in standard locations.
    fn scan_conversation_files() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Droid transcripts are stored in ~/.factory/sessions/<project-dir>/<uuid>.jsonl
        let search_dirs = vec![dirs::home_dir().map(|p| p.join(".factory/sessions"))];

        for dir_opt in search_dirs {
            if let Some(sessions_dir) = dir_opt
                && sessions_dir.exists()
            {
                // Recursively scan all project directories under sessions/
                Self::scan_jsonl_recursive(&sessions_dir, &mut paths);
            }
        }

        paths
    }

    /// Recursively scan directory for *.jsonl files (excluding .settings.json).
    fn scan_jsonl_recursive(dir: &Path, paths: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::scan_jsonl_recursive(&path, paths);
            } else if path.is_file()
                && path.extension().map(|ext| ext == "jsonl").unwrap_or(false)
                && !path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.contains(".settings."))
                    .unwrap_or(false)
            {
                paths.push(path);
            }
        }
    }

    /// Extract session ID from a Droid conversation file path.
    ///
    /// Droid files are typically named like: `<uuid>.jsonl`
    fn extract_session_id(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| format!("droid:{}", s))
    }
}

impl Agent for DroidAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        // Poll every 30 minutes for new Droid conversations
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
                agent_type: "droid".to_string(),
                transcript_path: path,
                transcript_format: TranscriptFormat::DroidJsonl,
                watermark_type: WatermarkType::Hybrid,
                initial_watermark: Box::new(HybridWatermark::new(0, 0, None)),
                model: None,
                tool: Some("Droid".to_string()),
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
        // Migrated from formats/droid.rs (will be removed in Phase 9)
        use crate::metrics::events::AgentTraceValues;
        use std::fs::File;
        use std::io::{BufRead, BufReader, Seek, SeekFrom};

        // Downcast watermark to HybridWatermark
        let hybrid_watermark = watermark
            .as_any()
            .downcast_ref::<HybridWatermark>()
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "Droid reader requires HybridWatermark, got incompatible type for session {}",
                    session_id
                ),
            })?;

        let start_offset = hybrid_watermark.offset;
        let mut record_count = hybrid_watermark.record;

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
        let mut latest_timestamp: Option<chrono::DateTime<chrono::Utc>> =
            hybrid_watermark.timestamp;

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

            // Only process "message" entries; skip session_start, todo_state, etc.
            if entry["type"].as_str() != Some("message") {
                continue;
            }

            // Track record count for hybrid watermark
            record_count += 1;

            // Extract timestamp
            let timestamp_opt = entry["timestamp"].as_str().and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.timestamp() as u64)
            });

            // Update latest_timestamp for hybrid watermark
            if let Some(ts_str) = entry["timestamp"].as_str()
                && let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts_str)
            {
                let utc_dt = dt.with_timezone(&chrono::Utc);
                if latest_timestamp.is_none() || Some(utc_dt) > latest_timestamp {
                    latest_timestamp = Some(utc_dt);
                }
            }

            let message = &entry["message"];
            let role = match message["role"].as_str() {
                Some(r) => r,
                None => continue,
            };

            // Extract events based on role
            match role {
                "user" => {
                    // User message - extract text content
                    let text = if let Some(content_array) = message["content"].as_array() {
                        let mut texts = Vec::new();
                        for item in content_array {
                            // Skip tool_result items - those are system-generated responses
                            if item["type"].as_str() == Some("tool_result") {
                                continue;
                            }
                            if item["type"].as_str() == Some("text")
                                && let Some(text) = item["text"].as_str()
                                && !text.trim().is_empty()
                            {
                                texts.push(text.to_string());
                            }
                        }
                        texts.join("\n")
                    } else if let Some(content) = message["content"].as_str() {
                        content.to_string()
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
                "assistant" => {
                    // Assistant message - can contain text, thinking, and tool_use
                    if let Some(content_array) = message["content"].as_array() {
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
                                        let tool_use_id =
                                            item["id"].as_str().map(|s| s.to_string());

                                        let mut event = AgentTraceValues::new()
                                            .event_type("tool_use")
                                            .tool_name(name);

                                        if let Some(id) = tool_use_id {
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

        // Create new hybrid watermark with updated offset, record count, and timestamp
        let new_watermark = Box::new(HybridWatermark::new(
            current_offset,
            record_count,
            latest_timestamp,
        ));

        // Droid doesn't store model in JSONL - it comes from .settings.json
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
    fn test_extract_session_id() {
        let path = PathBuf::from("/home/user/.factory/sessions/project-name/abc-123.jsonl");
        let session_id = DroidAgent::extract_session_id(&path);
        assert_eq!(session_id, Some("droid:abc-123".to_string()));
    }

    #[test]
    fn test_sweep_strategy() {
        let agent = DroidAgent;
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
            r#"{{"type":"message","timestamp":"2025-01-01T00:00:00Z","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"message","timestamp":"2025-01-01T00:00:01Z","message":{{"role":"assistant","content":[{{"type":"text","text":"Hi there"}}]}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = DroidAgent;
        let watermark = Box::new(HybridWatermark::new(0, 0, None));
        let result = agent
            .read_incremental(file.path(), watermark, "test-session")
            .unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.model, None); // Droid doesn't have model in JSONL

        // Verify hybrid watermark was updated
        let new_watermark = result
            .new_watermark
            .as_any()
            .downcast_ref::<HybridWatermark>()
            .unwrap();
        assert!(new_watermark.offset > 0); // Byte offset advanced
        assert_eq!(new_watermark.record, 2); // Two message records processed
        assert!(new_watermark.timestamp.is_some()); // Timestamp captured
    }
}
