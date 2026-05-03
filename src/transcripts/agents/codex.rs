//! Codex agent implementation with sweep discovery.

use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy, TranscriptFormat};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy, WatermarkType};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Codex agent that reads Codex JSONL transcript files.
pub struct CodexAgent {
    batch_size: usize,
}

impl CodexAgent {
    pub fn new() -> Self {
        Self { batch_size: 1000 }
    }

    #[cfg(test)]
    pub fn with_batch_size(batch_size: usize) -> Self {
        Self { batch_size }
    }

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

    fn scan_session_files() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        let codex_home = match dirs::home_dir() {
            Some(home) => home.join(".codex"),
            None => return paths,
        };

        for subdir in &["sessions", "archived_sessions"] {
            let search_dir = codex_home.join(subdir);
            if search_dir.exists() {
                Self::scan_rollout_recursive(&search_dir, &mut paths);
            }
        }

        paths
    }

    fn scan_rollout_recursive(dir: &Path, paths: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::scan_rollout_recursive(&path, paths);
            } else if path.is_file()
                && path.extension().map(|ext| ext == "jsonl").unwrap_or(false)
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("rollout-"))
                    .unwrap_or(false)
            {
                paths.push(path);
            }
        }
    }

    fn extract_session_id(path: &Path) -> Option<String> {
        let stem = path.file_stem()?.to_str()?;
        // Filename: rollout-2026-02-06T20-35-49-019c35bd-ad8e-7422-834c-3605bc4ee7ac
        // UUID is always 36 chars (with hyphens) at the end
        if stem.len() >= 36 {
            let uuid = &stem[stem.len() - 36..];
            Some(format!("codex:{}", uuid))
        } else {
            None
        }
    }
}

impl Default for CodexAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent for CodexAgent {
    fn batch_size_hint(&self) -> usize {
        self.batch_size
    }

    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        let paths = Self::scan_session_files();
        let mut sessions = Vec::new();

        for path in paths {
            let Some(session_id) = Self::extract_session_id(&path) else {
                continue;
            };

            sessions.push(DiscoveredSession {
                session_id,
                agent_type: "codex".to_string(),
                transcript_path: path,
                transcript_format: TranscriptFormat::CodexJsonl,
                watermark_type: WatermarkType::ByteOffset,
                initial_watermark: Box::new(ByteOffsetWatermark::new(0)),
                model: None,
                tool: Some("Codex".to_string()),
                external_thread_id: None,
            });
        }

        Ok(sessions)
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

            let entry: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        line = line_number,
                        path = %path.display(),
                        error = %e,
                        "skipping malformed JSON line"
                    );
                    continue;
                }
            };

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
        let agent = CodexAgent::new();
        assert_eq!(
            agent.sweep_strategy(),
            SweepStrategy::Periodic(Duration::from_secs(30 * 60))
        );
    }

    fn make_jsonl_line(i: usize) -> String {
        format!(
            r#"{{"type":"response_item","id":{},"payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"msg-{}"}}]}}}}"#,
            i, i
        )
    }

    fn drain_all(
        agent: &CodexAgent,
        path: &Path,
    ) -> (Vec<serde_json::Value>, Box<dyn WatermarkStrategy>) {
        let mut all = Vec::new();
        let mut wm: Box<dyn WatermarkStrategy> = Box::new(ByteOffsetWatermark::new(0));
        loop {
            let batch = agent.read_incremental(path, wm, "test").unwrap();
            if batch.events.is_empty() {
                wm = batch.new_watermark;
                break;
            }
            all.extend(batch.events);
            wm = batch.new_watermark;
        }
        (all, wm)
    }

    #[test]
    fn test_batch_resume_no_loss_or_repeat() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        for i in 0..5 {
            writeln!(file, "{}", make_jsonl_line(i)).unwrap();
        }
        file.flush().unwrap();

        let agent = CodexAgent::with_batch_size(2);
        let (events, _) = drain_all(&agent, file.path());

        assert_eq!(events.len(), 5);
        let ids: Vec<u64> = events.iter().map(|e| e["id"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn test_append_one_record_after_full_read() {
        use std::fs::OpenOptions;
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        for i in 0..3 {
            writeln!(file, "{}", make_jsonl_line(i)).unwrap();
        }
        file.flush().unwrap();

        let agent = CodexAgent::with_batch_size(2);
        let (all, wm) = drain_all(&agent, file.path());
        assert_eq!(all.len(), 3);

        let mut f = OpenOptions::new().append(true).open(file.path()).unwrap();
        writeln!(f, "{}", make_jsonl_line(3)).unwrap();
        f.flush().unwrap();

        let batch = agent.read_incremental(file.path(), wm, "test").unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0]["id"].as_u64().unwrap(), 3);
    }

    #[test]
    fn test_append_several_records_after_full_read() {
        use std::fs::OpenOptions;
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        for i in 0..3 {
            writeln!(file, "{}", make_jsonl_line(i)).unwrap();
        }
        file.flush().unwrap();

        let agent = CodexAgent::with_batch_size(2);
        let (_, mut wm) = drain_all(&agent, file.path());

        let mut f = OpenOptions::new().append(true).open(file.path()).unwrap();
        for i in 3..6 {
            writeln!(f, "{}", make_jsonl_line(i)).unwrap();
        }
        f.flush().unwrap();

        let mut new_events = Vec::new();
        loop {
            let batch = agent.read_incremental(file.path(), wm, "test").unwrap();
            wm = batch.new_watermark;
            if batch.events.is_empty() {
                break;
            }
            new_events.extend(batch.events);
        }
        assert_eq!(new_events.len(), 3);
        let ids: Vec<u64> = new_events
            .iter()
            .map(|e| e["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![3, 4, 5]);
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

        let agent = CodexAgent::new();
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

        let agent = CodexAgent::new();
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
