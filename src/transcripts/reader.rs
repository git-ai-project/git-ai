//! Generic incremental transcript readers.
//!
//! Two strategies cover all agents:
//! - `read_jsonl_incremental`: seek to byte offset, read N lines (covers 9/11 agents)
//! - `read_json_array_incremental`: parse JSON array, skip N records (covers amp, continue, copilot sessions)

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug)]
pub enum TranscriptError {
    NotFound(String),
    Io(String),
    Parse { line: usize, message: String },
}

impl std::fmt::Display for TranscriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(p) => write!(f, "transcript not found: {}", p),
            Self::Io(msg) => write!(f, "transcript I/O error: {}", msg),
            Self::Parse { line, message } => write!(f, "parse error at line {}: {}", line, message),
        }
    }
}

impl std::error::Error for TranscriptError {}

/// Result of reading a batch of transcript events.
pub struct TranscriptBatch {
    pub events: Vec<serde_json::Value>,
    /// New byte offset (for JSONL) or record index (for JSON array) after this batch.
    pub new_position: u64,
}

/// Read up to `batch_size` events from a JSONL file starting at `byte_offset`.
///
/// Returns the events and the new byte offset. Call repeatedly until
/// `events` is empty to drain the file.
///
/// Malformed lines are skipped (logged in debug builds).
pub fn read_jsonl_incremental(
    path: &Path,
    byte_offset: u64,
    batch_size: usize,
) -> Result<TranscriptBatch, TranscriptError> {
    let file = File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            TranscriptError::NotFound(path.display().to_string())
        } else {
            TranscriptError::Io(format!("{}: {}", path.display(), e))
        }
    })?;

    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(byte_offset))
        .map_err(|e| TranscriptError::Io(format!("seek to {}: {}", byte_offset, e)))?;

    let mut events = Vec::with_capacity(batch_size.min(256));
    let mut current_offset = byte_offset;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader
            .read_line(&mut line)
            .map_err(|e| TranscriptError::Io(format!("read line: {}", e)))?;

        if bytes_read == 0 {
            break;
        }

        current_offset += bytes_read as u64;

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(v) => events.push(v),
            Err(_) => continue,
        }

        if events.len() >= batch_size {
            break;
        }
    }

    Ok(TranscriptBatch {
        events,
        new_position: current_offset,
    })
}

/// Read up to `batch_size` events from a JSON array file, skipping `record_index` records.
///
/// Used for agents that store transcripts as a single JSON array (amp, continue-cli,
/// copilot session files).
pub fn read_json_array_incremental(
    path: &Path,
    record_index: u64,
    batch_size: usize,
) -> Result<TranscriptBatch, TranscriptError> {
    let mut file = File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            TranscriptError::NotFound(path.display().to_string())
        } else {
            TranscriptError::Io(format!("{}: {}", path.display(), e))
        }
    })?;

    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|e| TranscriptError::Io(format!("read {}: {}", path.display(), e)))?;

    let parsed: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| TranscriptError::Parse {
            line: 0,
            message: format!("invalid JSON: {}", e),
        })?;

    let array = match parsed.as_array() {
        Some(arr) => arr,
        None => {
            // Some agents wrap in an object with a known key
            parsed
                .get("messages")
                .or_else(|| parsed.get("events"))
                .or_else(|| parsed.get("thread"))
                .or_else(|| parsed.get("history"))
                .and_then(|v| v.as_array())
                .ok_or_else(|| TranscriptError::Parse {
                    line: 0,
                    message:
                        "expected JSON array or object with messages/events/thread/history key"
                            .into(),
                })?
        }
    };

    let skip = record_index as usize;
    let events: Vec<serde_json::Value> =
        array.iter().skip(skip).take(batch_size).cloned().collect();

    let new_index = skip as u64 + events.len() as u64;

    Ok(TranscriptBatch {
        events,
        new_position: new_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_jsonl_basic_read() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"role":"user","text":"hello"}}"#).unwrap();
        writeln!(f, r#"{{"role":"assistant","text":"hi"}}"#).unwrap();
        f.flush().unwrap();

        let batch = read_jsonl_incremental(f.path(), 0, 100).unwrap();
        assert_eq!(batch.events.len(), 2);
        assert_eq!(batch.events[0]["role"], "user");
        assert_eq!(batch.events[1]["role"], "assistant");
    }

    #[test]
    fn test_jsonl_incremental_resume() {
        let mut f = NamedTempFile::new().unwrap();
        for i in 0..5 {
            writeln!(f, r#"{{"id":{}}}"#, i).unwrap();
        }
        f.flush().unwrap();

        // Read first 2
        let batch1 = read_jsonl_incremental(f.path(), 0, 2).unwrap();
        assert_eq!(batch1.events.len(), 2);
        assert_eq!(batch1.events[0]["id"], 0);
        assert_eq!(batch1.events[1]["id"], 1);

        // Resume from where we left off
        let batch2 = read_jsonl_incremental(f.path(), batch1.new_position, 2).unwrap();
        assert_eq!(batch2.events.len(), 2);
        assert_eq!(batch2.events[0]["id"], 2);
        assert_eq!(batch2.events[1]["id"], 3);

        // Get the last one
        let batch3 = read_jsonl_incremental(f.path(), batch2.new_position, 2).unwrap();
        assert_eq!(batch3.events.len(), 1);
        assert_eq!(batch3.events[0]["id"], 4);

        // Nothing left
        let batch4 = read_jsonl_incremental(f.path(), batch3.new_position, 2).unwrap();
        assert!(batch4.events.is_empty());
    }

    #[test]
    fn test_jsonl_skips_malformed_lines() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"id":1}}"#).unwrap();
        writeln!(f, "not valid json {{{{").unwrap();
        writeln!(f, r#"{{"id":2}}"#).unwrap();
        f.flush().unwrap();

        let batch = read_jsonl_incremental(f.path(), 0, 100).unwrap();
        assert_eq!(batch.events.len(), 2);
        assert_eq!(batch.events[0]["id"], 1);
        assert_eq!(batch.events[1]["id"], 2);
    }

    #[test]
    fn test_jsonl_skips_empty_lines() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"id":1}}"#).unwrap();
        writeln!(f).unwrap();
        writeln!(f, "   ").unwrap();
        writeln!(f, r#"{{"id":2}}"#).unwrap();
        f.flush().unwrap();

        let batch = read_jsonl_incremental(f.path(), 0, 100).unwrap();
        assert_eq!(batch.events.len(), 2);
    }

    #[test]
    fn test_jsonl_file_not_found() {
        let result = read_jsonl_incremental(Path::new("/nonexistent/file.jsonl"), 0, 10);
        assert!(matches!(result, Err(TranscriptError::NotFound(_))));
    }

    #[test]
    fn test_jsonl_append_after_read() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"id":0}}"#).unwrap();
        writeln!(f, r#"{{"id":1}}"#).unwrap();
        f.flush().unwrap();

        let batch1 = read_jsonl_incremental(f.path(), 0, 100).unwrap();
        assert_eq!(batch1.events.len(), 2);

        // Append more data
        use std::fs::OpenOptions;
        let mut appender = OpenOptions::new().append(true).open(f.path()).unwrap();
        writeln!(appender, r#"{{"id":2}}"#).unwrap();
        appender.flush().unwrap();

        // Resume picks up new data
        let batch2 = read_jsonl_incremental(f.path(), batch1.new_position, 100).unwrap();
        assert_eq!(batch2.events.len(), 1);
        assert_eq!(batch2.events[0]["id"], 2);
    }

    #[test]
    fn test_json_array_basic() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, r#"[{{"id":0}},{{"id":1}},{{"id":2}}]"#).unwrap();
        f.flush().unwrap();

        let batch = read_json_array_incremental(f.path(), 0, 100).unwrap();
        assert_eq!(batch.events.len(), 3);
        assert_eq!(batch.new_position, 3);
    }

    #[test]
    fn test_json_array_incremental() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, r#"[{{"id":0}},{{"id":1}},{{"id":2}},{{"id":3}}]"#).unwrap();
        f.flush().unwrap();

        let batch1 = read_json_array_incremental(f.path(), 0, 2).unwrap();
        assert_eq!(batch1.events.len(), 2);
        assert_eq!(batch1.events[0]["id"], 0);
        assert_eq!(batch1.new_position, 2);

        let batch2 = read_json_array_incremental(f.path(), batch1.new_position, 2).unwrap();
        assert_eq!(batch2.events.len(), 2);
        assert_eq!(batch2.events[0]["id"], 2);
        assert_eq!(batch2.new_position, 4);

        let batch3 = read_json_array_incremental(f.path(), batch2.new_position, 2).unwrap();
        assert!(batch3.events.is_empty());
    }

    #[test]
    fn test_json_array_with_messages_key() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, r#"{{"messages":[{{"id":0}},{{"id":1}}]}}"#).unwrap();
        f.flush().unwrap();

        let batch = read_json_array_incremental(f.path(), 0, 100).unwrap();
        assert_eq!(batch.events.len(), 2);
    }

    #[test]
    fn test_json_array_with_thread_key() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, r#"{{"thread":[{{"id":0}}]}}"#).unwrap();
        f.flush().unwrap();

        let batch = read_json_array_incremental(f.path(), 0, 100).unwrap();
        assert_eq!(batch.events.len(), 1);
    }
}
