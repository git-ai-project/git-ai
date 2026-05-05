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
pub struct CopilotAgent {
    batch_size: usize,
}

impl CopilotAgent {
    pub fn new() -> Self {
        Self { batch_size: 1000 }
    }

    #[cfg(test)]
    pub fn with_batch_size(batch_size: usize) -> Self {
        Self { batch_size }
    }

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

impl Default for CopilotAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent for CopilotAgent {
    fn batch_size_hint(&self) -> usize {
        self.batch_size
    }

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
        let agent = CopilotAgent::new();
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

    // -- Event stream (JSONL / ByteOffset) batch-resume tests --

    fn make_event_stream_line(i: usize) -> String {
        format!(
            r#"{{"type":"user.message","id":{},"data":{{"content":"msg-{}"}},"timestamp":"2025-01-01T00:00:{:02}Z"}}"#,
            i, i, i
        )
    }

    fn drain_event_stream(
        agent: &CopilotAgent,
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
    fn test_event_stream_batch_resume_no_loss_or_repeat() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        for i in 0..5 {
            writeln!(file, "{}", make_event_stream_line(i)).unwrap();
        }
        file.flush().unwrap();

        let agent = CopilotAgent::with_batch_size(2);
        let (events, _) = drain_event_stream(&agent, file.path());

        assert_eq!(events.len(), 5);
        let ids: Vec<u64> = events.iter().map(|e| e["id"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn test_event_stream_append_one_after_full_read() {
        use std::fs::OpenOptions;
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        for i in 0..3 {
            writeln!(file, "{}", make_event_stream_line(i)).unwrap();
        }
        file.flush().unwrap();

        let agent = CopilotAgent::with_batch_size(2);
        let (all, wm) = drain_event_stream(&agent, file.path());
        assert_eq!(all.len(), 3);

        let mut f = OpenOptions::new().append(true).open(file.path()).unwrap();
        writeln!(f, "{}", make_event_stream_line(3)).unwrap();
        f.flush().unwrap();

        let batch = agent.read_incremental(file.path(), wm, "test").unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0]["id"].as_u64().unwrap(), 3);
    }

    #[test]
    fn test_event_stream_append_several_after_full_read() {
        use std::fs::OpenOptions;
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        for i in 0..3 {
            writeln!(file, "{}", make_event_stream_line(i)).unwrap();
        }
        file.flush().unwrap();

        let agent = CopilotAgent::with_batch_size(2);
        let (_, mut wm) = drain_event_stream(&agent, file.path());

        let mut f = OpenOptions::new().append(true).open(file.path()).unwrap();
        for i in 3..6 {
            writeln!(f, "{}", make_event_stream_line(i)).unwrap();
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

    // -- Session JSON (RecordIndex) batch-resume tests --

    fn make_session_json(request_count: usize) -> String {
        let requests: Vec<String> = (0..request_count)
            .map(|i| {
                format!(
                    r#"{{"id":{},"message":{{"text":"msg-{}"}},"response":[{{"kind":"markdownContent","value":"reply-{}"}}]}}"#,
                    i, i, i
                )
            })
            .collect();
        format!(r#"{{"requests":[{}]}}"#, requests.join(","))
    }

    fn drain_session_json(
        agent: &CopilotAgent,
        path: &Path,
    ) -> (Vec<serde_json::Value>, Box<dyn WatermarkStrategy>) {
        let mut all = Vec::new();
        let mut wm: Box<dyn WatermarkStrategy> = Box::new(RecordIndexWatermark::new(0));
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
    fn test_session_json_batch_resume_no_loss_or_repeat() {
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::with_suffix(".json").unwrap();
        std::io::Write::write_all(&mut file, make_session_json(5).as_bytes()).unwrap();
        std::io::Write::flush(&mut file).unwrap();

        let agent = CopilotAgent::with_batch_size(2);
        let (events, _) = drain_session_json(&agent, file.path());

        assert_eq!(events.len(), 5);
        let ids: Vec<u64> = events.iter().map(|e| e["id"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn test_session_json_append_one_after_full_read() {
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::with_suffix(".json").unwrap();
        std::io::Write::write_all(&mut file, make_session_json(3).as_bytes()).unwrap();
        std::io::Write::flush(&mut file).unwrap();

        let agent = CopilotAgent::with_batch_size(2);
        let (all, wm) = drain_session_json(&agent, file.path());
        assert_eq!(all.len(), 3);

        // Rewrite file with 4 requests (simulating append in JSON format)
        std::fs::write(file.path(), make_session_json(4)).unwrap();

        let batch = agent.read_incremental(file.path(), wm, "test").unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0]["id"].as_u64().unwrap(), 3);
    }

    #[test]
    fn test_session_json_append_several_after_full_read() {
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::with_suffix(".json").unwrap();
        std::io::Write::write_all(&mut file, make_session_json(3).as_bytes()).unwrap();
        std::io::Write::flush(&mut file).unwrap();

        let agent = CopilotAgent::with_batch_size(2);
        let (_, mut wm) = drain_session_json(&agent, file.path());

        // Rewrite file with 6 requests
        std::fs::write(file.path(), make_session_json(6)).unwrap();

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

        let agent = CopilotAgent::new();
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

        let agent = CopilotAgent::new();
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
