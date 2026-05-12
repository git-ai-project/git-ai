use crate::authorship::authorship_log_serialization::generate_session_id;
use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy, TranscriptFormat};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy, WatermarkType};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct CopilotCliAgent {
    batch_size: usize,
}

impl CopilotCliAgent {
    pub fn new() -> Self {
        Self { batch_size: 1000 }
    }

    #[cfg(test)]
    pub fn with_batch_size(batch_size: usize) -> Self {
        Self { batch_size }
    }

    fn session_state_base_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".copilot/session-state"))
    }
}

impl Default for CopilotCliAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent for CopilotCliAgent {
    fn batch_size_hint(&self) -> usize {
        self.batch_size
    }

    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        let Some(base_dir) = Self::session_state_base_dir() else {
            return Ok(Vec::new());
        };

        if !base_dir.exists() {
            return Ok(Vec::new());
        }

        let entries = fs::read_dir(&base_dir).map_err(|e| TranscriptError::Transient {
            message: format!("Failed to read copilot session-state dir: {}", e),
            retry_after: Duration::from_secs(60),
        })?;

        let mut sessions = Vec::new();

        for entry in entries.flatten() {
            let dir_path = entry.path();
            if !dir_path.is_dir() {
                continue;
            }

            let events_path = dir_path.join("events.jsonl");
            if !events_path.exists() {
                continue;
            }

            let Some(external_session_id) = dir_path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
            else {
                continue;
            };

            let session_id = generate_session_id(&external_session_id, "github-copilot-cli");

            sessions.push(DiscoveredSession {
                session_id,
                tool: "github-copilot-cli".to_string(),
                transcript_path: events_path,
                transcript_format: TranscriptFormat::CopilotCliSessionJsonl,
                watermark_type: WatermarkType::ByteOffset,
                initial_watermark: Box::new(ByteOffsetWatermark::new(0)),
                external_session_id,
                external_parent_session_id: None,
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
        use std::fs::File;
        use std::io::{BufRead, BufReader, Seek, SeekFrom};

        let byte_watermark = watermark
            .as_any()
            .downcast_ref::<ByteOffsetWatermark>()
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "CopilotCliAgent requires ByteOffsetWatermark, got incompatible type for session {}",
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
                    retry_after: Duration::from_secs(5),
                }
            }
        })?;

        let mut reader = BufReader::new(file);

        reader
            .seek(SeekFrom::Start(start_offset))
            .map_err(|e| TranscriptError::Transient {
                message: format!("Failed to seek to offset {}: {}", start_offset, e),
                retry_after: Duration::from_secs(5),
            })?;

        let batch_limit = self.batch_size_hint();
        let mut events = Vec::with_capacity(batch_limit);
        let mut current_offset = start_offset;

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

            current_offset += bytes_read as u64;

            if line.trim().is_empty() {
                continue;
            }

            let entry: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
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

    fn extract_event_ids(
        &self,
        event: &serde_json::Value,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let id = event.get("id").and_then(|v| v.as_str()).map(String::from);
        let parent_id = event
            .get("parentId")
            .and_then(|v| v.as_str())
            .map(String::from);
        (id, parent_id, None)
    }

    fn infer_cwd(&self, transcript_path: &Path) -> Option<PathBuf> {
        use std::io::{BufRead, BufReader};

        let file = fs::File::open(transcript_path).ok()?;
        let reader = BufReader::new(file);

        for line in reader.lines().map_while(Result::ok).take(5) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some(json) = serde_json::from_str::<serde_json::Value>(trimmed).ok() else {
                continue;
            };
            if json.get("type").and_then(|v| v.as_str()) == Some("session.start") {
                return json
                    .get("data")
                    .and_then(|d| d.get("context"))
                    .and_then(|c| c.get("cwd"))
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sweep_strategy() {
        let agent = CopilotCliAgent::new();
        assert_eq!(
            agent.sweep_strategy(),
            SweepStrategy::Periodic(Duration::from_secs(30 * 60))
        );
    }

    fn make_event_line(i: usize) -> String {
        format!(
            r#"{{"type":"user.message","data":{{"content":"msg-{}"}},"id":"evt-{}","timestamp":"2026-05-12T00:00:{:02}Z","parentId":null}}"#,
            i, i, i
        )
    }

    fn drain_all(
        agent: &CopilotCliAgent,
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

        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        for i in 0..5 {
            writeln!(file, "{}", make_event_line(i)).unwrap();
        }
        file.flush().unwrap();

        let agent = CopilotCliAgent::with_batch_size(2);
        let (events, _) = drain_all(&agent, file.path());

        assert_eq!(events.len(), 5);
        let ids: Vec<String> = events
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, vec!["evt-0", "evt-1", "evt-2", "evt-3", "evt-4"]);
    }

    #[test]
    fn test_append_after_full_read() {
        use std::fs::OpenOptions;
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        for i in 0..3 {
            writeln!(file, "{}", make_event_line(i)).unwrap();
        }
        file.flush().unwrap();

        let agent = CopilotCliAgent::with_batch_size(2);
        let (all, wm) = drain_all(&agent, file.path());
        assert_eq!(all.len(), 3);

        let mut f = OpenOptions::new().append(true).open(file.path()).unwrap();
        writeln!(f, "{}", make_event_line(3)).unwrap();
        writeln!(f, "{}", make_event_line(4)).unwrap();
        f.flush().unwrap();

        let mut new_events = Vec::new();
        let mut wm = wm;
        loop {
            let batch = agent.read_incremental(file.path(), wm, "test").unwrap();
            wm = batch.new_watermark;
            if batch.events.is_empty() {
                break;
            }
            new_events.extend(batch.events);
        }
        assert_eq!(new_events.len(), 2);
        let ids: Vec<String> = new_events
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, vec!["evt-3", "evt-4"]);
    }

    #[test]
    fn test_infer_cwd() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        writeln!(
            file,
            r#"{{"type":"session.start","data":{{"sessionId":"test","context":{{"cwd":"/Users/test/myproject","gitRoot":"/Users/test/myproject"}}}},"id":"e1","timestamp":"2026-01-01T00:00:00Z","parentId":null}}"#
        )
        .unwrap();
        writeln!(file, "{}", make_event_line(0)).unwrap();
        file.flush().unwrap();

        let agent = CopilotCliAgent::new();
        let cwd = agent.infer_cwd(file.path());
        assert_eq!(cwd, Some(PathBuf::from("/Users/test/myproject")));
    }

    #[test]
    fn test_extract_event_ids() {
        let agent = CopilotCliAgent::new();
        let event = serde_json::json!({
            "type": "user.message",
            "id": "evt-123",
            "parentId": "evt-122",
            "data": {}
        });
        let (id, parent_id, tool_use_id) = agent.extract_event_ids(&event);
        assert_eq!(id, Some("evt-123".to_string()));
        assert_eq!(parent_id, Some("evt-122".to_string()));
        assert_eq!(tool_use_id, None);
    }

    #[test]
    fn test_extract_event_ids_null_parent() {
        let agent = CopilotCliAgent::new();
        let event = serde_json::json!({
            "type": "session.start",
            "id": "evt-001",
            "parentId": null,
            "data": {}
        });
        let (id, parent_id, _) = agent.extract_event_ids(&event);
        assert_eq!(id, Some("evt-001".to_string()));
        assert_eq!(parent_id, None);
    }
}
