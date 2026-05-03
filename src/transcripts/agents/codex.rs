//! Codex agent implementation with sweep discovery.

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

        let batch_limit = self.batch_size_hint();
        let mut events = Vec::with_capacity(batch_limit);
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

            events.push(entry);
            if events.len() >= batch_limit {
                break;
            }
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

        // Both JSONL lines are returned as raw JSON
        assert_eq!(result.events.len(), 2);
        assert_eq!(result.events[0]["type"], "turn_context");
        assert_eq!(result.events[0]["payload"]["model"], "gpt-4o");
        assert_eq!(result.events[1]["type"], "response_item");
        assert_eq!(result.events[1]["payload"]["role"], "assistant");
    }

    #[test]
    fn test_read_incremental_legacy_format() {
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

        // Both JSONL lines are returned as raw JSON
        assert_eq!(result.events.len(), 2);
        assert_eq!(result.events[0]["type"], "event_msg");
        assert_eq!(result.events[0]["payload"]["type"], "user_message");
        assert_eq!(result.events[1]["payload"]["type"], "agent_message");
    }
}
