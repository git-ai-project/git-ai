//! GitHub Copilot agent implementation with sweep discovery.

use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy, TranscriptFormat};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{
    ByteOffsetWatermark, RecordIndexWatermark, WatermarkStrategy, WatermarkType,
};
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

            // JSONL event streams use byte offset (seekable); session JSON uses
            // record index (count of processed requests).
            let (watermark_type, initial_watermark): (WatermarkType, Box<dyn WatermarkStrategy>) =
                if format == TranscriptFormat::CopilotEventStreamJsonl {
                    (
                        WatermarkType::ByteOffset,
                        Box::new(ByteOffsetWatermark::new(0)),
                    )
                } else {
                    (
                        WatermarkType::RecordIndex,
                        Box::new(RecordIndexWatermark::new(0)),
                    )
                };

            let session = DiscoveredSession {
                session_id,
                agent_type: "copilot".to_string(),
                transcript_path: path,
                transcript_format: format,
                watermark_type,
                initial_watermark,
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
        let batch_limit = self.batch_size_hint();
        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            read_event_stream(path, watermark, session_id, batch_limit)
        } else {
            read_session_json(path, watermark, session_id, batch_limit)
        }
    }
}

/// Read Copilot session JSON incrementally.
fn read_session_json(
    path: &Path,
    watermark: Box<dyn WatermarkStrategy>,
    session_id: &str,
    batch_limit: usize,
) -> Result<TranscriptBatch, TranscriptError> {
    let record_watermark = watermark
        .as_any()
        .downcast_ref::<RecordIndexWatermark>()
        .ok_or_else(|| TranscriptError::Fatal {
            message: format!(
                "Copilot session reader requires RecordIndexWatermark, got incompatible type for session {}",
                session_id
            ),
        })?;

    let skip_count = record_watermark.0 as usize;

    // Check if running in Codespaces or Remote Containers - if so, return empty transcript
    let is_codespaces = std::env::var("CODESPACES").ok().as_deref() == Some("true");
    let is_remote_containers = std::env::var("REMOTE_CONTAINERS").ok().as_deref() == Some("true");

    if is_codespaces || is_remote_containers {
        return Ok(TranscriptBatch {
            events: Vec::new(),
            new_watermark: watermark,
        });
    }

    let file = std::fs::File::open(path).map_err(|e| {
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

    let reader = std::io::BufReader::new(file);
    let mut session_json: serde_json::Value =
        serde_json::from_reader(reader).map_err(|e| TranscriptError::Parse {
            line: 0,
            message: format!("Invalid JSON in {}: {}", path.display(), e),
        })?;

    let requests = match session_json
        .as_object_mut()
        .and_then(|obj| obj.remove("requests"))
    {
        Some(serde_json::Value::Array(arr)) => arr,
        _ => {
            return Err(TranscriptError::Parse {
                line: 0,
                message: "requests array not found in Copilot session JSON".to_string(),
            });
        }
    };

    let events: Vec<serde_json::Value> = requests
        .into_iter()
        .skip(skip_count)
        .take(batch_limit)
        .collect();

    let new_watermark = Box::new(RecordIndexWatermark::new(
        (skip_count + events.len()) as u64,
    ));

    Ok(TranscriptBatch {
        events,
        new_watermark,
    })
}

/// Read Copilot event stream JSONL incrementally.
fn read_event_stream(
    path: &Path,
    watermark: Box<dyn WatermarkStrategy>,
    session_id: &str,
    batch_limit: usize,
) -> Result<TranscriptBatch, TranscriptError> {
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

    let mut events = Vec::with_capacity(batch_limit);
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

        events.push(entry);
        if events.len() >= batch_limit {
            break;
        }
    }

    // Create new watermark with updated offset
    let new_watermark = Box::new(ByteOffsetWatermark::new(current_offset));

    Ok(TranscriptBatch {
        events,
        new_watermark,
    })
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
        let watermark = Box::new(RecordIndexWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test-session")
            .unwrap();

        // Each request object is returned as a raw JSON event
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0]["message"]["text"], "Hello");
        assert_eq!(result.events[0]["response"][0]["kind"], "markdownContent");
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

        // Both JSONL lines are returned as raw JSON
        assert_eq!(result.events.len(), 2);
        assert_eq!(result.events[0]["type"], "user.message");
        assert_eq!(result.events[0]["data"]["content"], "Hello");
        assert_eq!(result.events[1]["type"], "assistant.message");
        assert_eq!(result.events[1]["data"]["modelId"], "copilot/gpt-4");
    }
}
