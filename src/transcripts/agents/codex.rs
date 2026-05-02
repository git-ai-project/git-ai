//! Codex agent implementation with sweep discovery.

use crate::metrics::events::AgentTraceValues;
use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Codex agent that reads Codex JSONL transcript files.
pub struct CodexAgent;

impl CodexAgent {
    /// Search for a rollout file matching the given session ID in the Codex home directory.
    ///
    /// Looks in both `sessions` and `archived_sessions` subdirectories for files
    /// matching `rollout-*{session_id}*.jsonl`. Returns the newest match by
    /// modification time.
    pub fn find_rollout_path_for_session_in_home(
        session_id: &str,
        codex_home: &Path,
    ) -> Result<Option<PathBuf>, TranscriptError> {
        let mut candidates: Vec<PathBuf> = Vec::new();

        for subdir in &["sessions", "archived_sessions"] {
            let search_dir = codex_home.join(subdir);
            if !search_dir.exists() {
                continue;
            }

            let pattern = format!("{}/**/rollout-*{}*.jsonl", search_dir.display(), session_id);

            let entries = glob::glob(&pattern).map_err(|e| TranscriptError::Fatal {
                message: format!("Invalid glob pattern for Codex session search: {}", e),
            })?;

            for entry in entries {
                let path = entry.map_err(|e| TranscriptError::Fatal {
                    message: format!("Error reading glob entry: {}", e),
                })?;
                candidates.push(path);
            }
        }

        if candidates.is_empty() {
            return Ok(None);
        }

        // Return the newest by modification time
        let newest = candidates
            .into_iter()
            .filter_map(|p| {
                p.metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| (p, t))
            })
            .max_by_key(|(_, t)| *t)
            .map(|(p, _)| p);

        Ok(newest)
    }
}

impl Agent for CodexAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        // Discovery comes from presets for now
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
                    "Codex reader requires ByteOffsetWatermark, got incompatible type for session {}",
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

        // Read all new lines into a buffer for two-pass processing
        let mut parsed_lines: Vec<serde_json::Value> = Vec::new();
        let mut current_offset = start_offset;
        let mut line_number = 0;
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
                break;
            }

            line_number += 1;
            current_offset += bytes_read as u64;

            if line.trim().is_empty() {
                continue;
            }

            let entry: serde_json::Value =
                serde_json::from_str(&line).map_err(|e| TranscriptError::Parse {
                    line: line_number,
                    message: format!("Invalid JSON in {}: {}", path.display(), e),
                })?;

            parsed_lines.push(entry);
        }

        // Pass 1: Primary format processing
        let mut events = Vec::new();
        let mut model = None;

        for entry in &parsed_lines {
            let timestamp_opt = entry["timestamp"].as_str().and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.timestamp() as u64)
            });

            let entry_type = entry["type"].as_str().unwrap_or("");
            let payload = if entry["payload"].is_object() {
                &entry["payload"]
            } else {
                entry
            };

            match entry_type {
                "turn_context" => {
                    if let Some(m) = payload["model"].as_str() {
                        model = Some(m.to_string());
                    }
                }
                "response_item" => {
                    let response_type = payload["type"].as_str().unwrap_or("");
                    match response_type {
                        "message" => {
                            let role = payload["role"].as_str().unwrap_or("");
                            if let Some(content_array) = payload["content"].as_array() {
                                match role {
                                    "user" => {
                                        let texts: Vec<&str> = content_array
                                            .iter()
                                            .filter(|item| {
                                                item["type"].as_str() == Some("input_text")
                                            })
                                            .filter_map(|item| item["text"].as_str())
                                            .collect();
                                        let text = texts.join("\n");
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
                                        let texts: Vec<&str> = content_array
                                            .iter()
                                            .filter(|item| {
                                                item["type"].as_str() == Some("output_text")
                                            })
                                            .filter_map(|item| item["text"].as_str())
                                            .collect();
                                        let text = texts.join("\n");
                                        if !text.trim().is_empty() {
                                            let mut event = AgentTraceValues::new()
                                                .event_type("assistant_message")
                                                .response_text(text);
                                            if let Some(ts) = timestamp_opt {
                                                event = event.event_ts(ts);
                                            }
                                            events.push(event);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "function_call" | "custom_tool_call" | "local_shell_call"
                        | "web_search_call" => {
                            let name = payload["name"].as_str().unwrap_or(response_type);
                            let mut event = AgentTraceValues::new()
                                .event_type("tool_use")
                                .tool_name(name);
                            if let Some(ts) = timestamp_opt {
                                event = event.event_ts(ts);
                            }
                            events.push(event);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        // Pass 2: Legacy fallback - if no events from primary format, try legacy event_msg format
        if events.is_empty() {
            for entry in &parsed_lines {
                let timestamp_opt = entry["timestamp"].as_str().and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.timestamp() as u64)
                });

                let entry_type = entry["type"].as_str().unwrap_or("");
                if entry_type != "event_msg" {
                    continue;
                }

                let payload = if entry["payload"].is_object() {
                    &entry["payload"]
                } else {
                    entry
                };

                let payload_type = payload["type"].as_str().unwrap_or("");
                match payload_type {
                    "user_message" => {
                        if let Some(message) = payload["message"].as_str()
                            && !message.trim().is_empty()
                        {
                            let mut event = AgentTraceValues::new()
                                .event_type("user_message")
                                .prompt_text(message);
                            if let Some(ts) = timestamp_opt {
                                event = event.event_ts(ts);
                            }
                            events.push(event);
                        }
                    }
                    "agent_message" => {
                        if let Some(message) = payload["message"].as_str()
                            && !message.trim().is_empty()
                        {
                            let mut event = AgentTraceValues::new()
                                .event_type("assistant_message")
                                .response_text(message);
                            if let Some(ts) = timestamp_opt {
                                event = event.event_ts(ts);
                            }
                            events.push(event);
                        }
                    }
                    _ => {}
                }
            }
        }

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
    fn test_sweep_strategy() {
        let agent = CodexAgent;
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
            r#"{{"type":"turn_context","payload":{{"model":"gpt-4o"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Hello"}}]}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = CodexAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 1);
        assert_eq!(result.model, Some("gpt-4o".to_string()));
    }

    #[test]
    fn test_read_incremental_legacy_fallback() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"event_msg","payload":{{"type":"user_message","message":"Hello"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"Hi there"}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = CodexAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 2);
    }
}
