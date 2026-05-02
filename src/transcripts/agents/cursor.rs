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
        use std::fs::File;
        use std::io::{BufRead, BufReader, Seek, SeekFrom};

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

        reader
            .seek(SeekFrom::Start(start_offset))
            .map_err(|e| TranscriptError::Transient {
                message: format!("Failed to seek to offset {}: {}", start_offset, e),
                retry_after: std::time::Duration::from_secs(5),
            })?;

        let mut events = Vec::new();
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
        }

        let new_watermark = Box::new(ByteOffsetWatermark::new(current_offset));

        Ok(TranscriptBatch {
            events,
            new_watermark,
        })
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
        assert_eq!(result.events[0]["role"].as_str(), Some("user"));
        assert_eq!(result.events[1]["role"].as_str(), Some("assistant"));
    }
}
