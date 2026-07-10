//! Core types for transcript processing.

use std::io::{BufRead, Read};
use std::time::Duration;

/// Maximum retained size of one JSONL transcript record.
pub const MAX_JSONL_EVENT_BYTES: usize = 1024 * 1024;

/// Target maximum raw size of events retained in one transcript batch.
///
/// The record that crosses this threshold remains in the batch, so the hard
/// upper bound is this value plus [`MAX_JSONL_EVENT_BYTES`].
pub const MAX_JSONL_BATCH_BYTES: usize = 8 * 1024 * 1024;

/// Maximum size of vendor transcript formats that require whole-document JSON parsing.
pub const MAX_MONOLITHIC_JSON_BYTES: u64 = 32 * 1024 * 1024;

/// Maximum size of auxiliary JSON metadata consulted during transcript processing.
pub const MAX_JSON_METADATA_BYTES: u64 = 1024 * 1024;

pub fn read_bounded_json_file(
    path: &std::path::Path,
    kind: &str,
    max_bytes: u64,
) -> Result<serde_json::Value, StreamError> {
    let file = std::fs::File::open(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => StreamError::Fatal {
            message: format!("{kind} file not found: {}", path.display()),
        },
        std::io::ErrorKind::PermissionDenied => StreamError::Fatal {
            message: format!("permission denied reading {kind}: {}", path.display()),
        },
        _ => StreamError::Transient {
            message: format!("failed to open {kind} {}: {error}", path.display()),
            retry_after: Duration::from_secs(5),
        },
    })?;
    let size_bytes = file.metadata().map_err(|error| StreamError::Transient {
        message: format!("failed to inspect {kind} {}: {error}", path.display()),
        retry_after: Duration::from_secs(5),
    })?;
    if size_bytes.len() > max_bytes {
        return Err(StreamError::Fatal {
            message: format!(
                "{kind} exceeded the {max_bytes} byte limit ({}): {}",
                size_bytes.len(),
                path.display()
            ),
        });
    }

    let reader = std::io::BufReader::new(file.take(max_bytes.saturating_add(1)));
    serde_json::from_reader(reader).map_err(|error| StreamError::Parse {
        line: error.line(),
        message: format!("invalid JSON in {kind} {}: {error}", path.display()),
    })
}

pub fn read_monolithic_transcript_json(
    path: &std::path::Path,
) -> Result<serde_json::Value, StreamError> {
    read_bounded_json_file(path, "monolithic transcript", MAX_MONOLITHIC_JSON_BYTES)
}

#[derive(Default)]
pub struct JsonlReadStats {
    oversized_records: usize,
    oversized_bytes: usize,
}

impl JsonlReadStats {
    pub fn record_oversized(&mut self, bytes: usize) {
        self.oversized_records = self.oversized_records.saturating_add(1);
        self.oversized_bytes = self.oversized_bytes.saturating_add(bytes);
    }

    pub fn warn_if_oversized(&self, path: &std::path::Path) {
        if self.oversized_records > 0 {
            tracing::warn!(
                path = %path.display(),
                records = self.oversized_records,
                bytes = self.oversized_bytes,
                per_record_limit = MAX_JSONL_EVENT_BYTES,
                "skipped oversized JSONL records"
            );
        }
    }
}

/// Result of reading a single line from a JSONL reader.
pub enum JsonlLineState {
    /// End of file reached.
    Eof,
    /// Incomplete line (no trailing newline) — writer still appending.
    Partial,
    /// Complete line ready for processing. Contains bytes read.
    Complete(usize),
    /// Complete line that exceeded [`MAX_JSONL_EVENT_BYTES`] and was discarded.
    Oversized(usize),
}

/// Read a line from a BufReader, detecting partial writes from concurrent writers.
///
/// Returns `Eof` if no more data, `Partial` if the line lacks a trailing newline,
/// `Complete(bytes)` on success, or `Oversized(bytes)` after consuming and
/// discarding a complete record that exceeds [`MAX_JSONL_EVENT_BYTES`].
pub fn read_jsonl_line(
    reader: &mut impl BufRead,
    line: &mut String,
) -> std::io::Result<JsonlLineState> {
    let mut bytes = std::mem::take(line).into_bytes();
    bytes.clear();
    let mut bytes_read = 0usize;
    let mut complete = false;
    let mut oversized = false;

    loop {
        let consumed = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                break;
            }

            let consumed = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            complete = available[consumed - 1] == b'\n';
            bytes_read = bytes_read.saturating_add(consumed);

            if !oversized && bytes_read <= MAX_JSONL_EVENT_BYTES {
                bytes.extend_from_slice(&available[..consumed]);
            } else {
                oversized = true;
            }
            consumed
        };
        reader.consume(consumed);

        if complete {
            break;
        }
    }

    if bytes_read == 0 {
        *line = String::from_utf8(bytes).expect("empty byte buffer is valid UTF-8");
        return Ok(JsonlLineState::Eof);
    }

    if oversized {
        *line = String::new();
        return Ok(if complete {
            JsonlLineState::Oversized(bytes_read)
        } else {
            JsonlLineState::Partial
        });
    }

    *line = String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if !complete {
        return Ok(JsonlLineState::Partial);
    }
    Ok(JsonlLineState::Complete(bytes_read))
}

/// Whether a JSONL reader should return its current batch.
pub fn jsonl_batch_limit_reached(
    event_count: usize,
    event_limit: usize,
    batch_bytes: usize,
) -> bool {
    event_count >= event_limit || batch_bytes >= MAX_JSONL_BATCH_BYTES
}

/// Search a bounded number of complete JSONL records without retaining an
/// oversized record.
pub fn find_in_jsonl_lines<T>(
    reader: &mut impl BufRead,
    max_lines: usize,
    mut find: impl FnMut(&str) -> Option<T>,
) -> Option<T> {
    let mut line = String::new();
    for _ in 0..max_lines {
        match read_jsonl_line(reader, &mut line).ok()? {
            JsonlLineState::Complete(_) => {
                if let Some(value) = find(&line) {
                    return Some(value);
                }
            }
            JsonlLineState::Oversized(_) => continue,
            JsonlLineState::Eof | JsonlLineState::Partial => break,
        }
    }
    None
}

/// Errors that can occur during transcript processing.
#[derive(Debug, Clone)]
pub enum StreamError {
    /// Transient errors that should be retried (file locked, network timeout).
    Transient {
        message: String,
        retry_after: Duration,
    },
    /// Parse errors from malformed data (bad JSON, unexpected format).
    Parse { line: usize, message: String },
    /// Fatal errors that cannot be recovered (file deleted, permissions denied).
    Fatal { message: String },
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::Transient {
                message,
                retry_after,
            } => write!(
                f,
                "Transient error (retry after {:?}): {}",
                retry_after, message
            ),
            StreamError::Parse { line, message } => {
                write!(f, "Parse error at line {}: {}", line, message)
            }
            StreamError::Fatal { message } => write!(f, "Fatal error: {}", message),
        }
    }
}

impl std::error::Error for StreamError {}

/// Batch of transcript events returned by transcript readers after processing.
pub struct StreamBatch {
    /// Raw JSON events from the transcript.
    pub events: Vec<serde_json::Value>,
    /// Updated watermark position after processing this batch.
    pub new_watermark: Box<dyn crate::streams::WatermarkStrategy>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transient_error_display() {
        let err = StreamError::Transient {
            message: "file locked".to_string(),
            retry_after: Duration::from_secs(5),
        };
        let display = format!("{}", err);
        assert!(display.contains("Transient error"));
        assert!(display.contains("5s"));
        assert!(display.contains("file locked"));
    }

    #[test]
    fn test_parse_error_display() {
        let err = StreamError::Parse {
            line: 42,
            message: "invalid JSON".to_string(),
        };
        let display = format!("{}", err);
        assert!(display.contains("Parse error at line 42"));
        assert!(display.contains("invalid JSON"));
    }

    #[test]
    fn test_fatal_error_display() {
        let err = StreamError::Fatal {
            message: "file deleted".to_string(),
        };
        let display = format!("{}", err);
        assert!(display.contains("Fatal error"));
        assert!(display.contains("file deleted"));
    }

    #[test]
    fn test_error_is_std_error() {
        let err = StreamError::Fatal {
            message: "test".to_string(),
        };
        let _: &dyn std::error::Error = &err;
    }

    #[test]
    fn test_error_clone() {
        let err = StreamError::Transient {
            message: "test".to_string(),
            retry_after: Duration::from_secs(10),
        };
        let cloned = err.clone();
        match cloned {
            StreamError::Transient {
                message,
                retry_after,
            } => {
                assert_eq!(message, "test");
                assert_eq!(retry_after, Duration::from_secs(10));
            }
            _ => panic!("Expected Transient variant"),
        }
    }

    #[test]
    fn test_read_jsonl_line_eof() {
        let data = b"";
        let mut reader = std::io::BufReader::new(&data[..]);
        let mut line = String::new();
        let result = read_jsonl_line(&mut reader, &mut line).unwrap();
        assert!(matches!(result, JsonlLineState::Eof));
    }

    #[test]
    fn test_read_jsonl_line_complete() {
        let data = b"{\"id\":1}\n";
        let mut reader = std::io::BufReader::new(&data[..]);
        let mut line = String::new();
        let result = read_jsonl_line(&mut reader, &mut line).unwrap();
        assert!(matches!(result, JsonlLineState::Complete(9)));
        assert_eq!(line, "{\"id\":1}\n");
    }

    #[test]
    fn test_read_jsonl_line_partial() {
        let data = b"{\"id\":1}";
        let mut reader = std::io::BufReader::new(&data[..]);
        let mut line = String::new();
        let result = read_jsonl_line(&mut reader, &mut line).unwrap();
        assert!(matches!(result, JsonlLineState::Partial));
    }

    #[test]
    fn test_read_jsonl_line_multiple_lines() {
        let data = b"{\"a\":1}\n{\"b\":2}\n";
        let mut reader = std::io::BufReader::new(&data[..]);
        let mut line = String::new();

        let r1 = read_jsonl_line(&mut reader, &mut line).unwrap();
        assert!(matches!(r1, JsonlLineState::Complete(8)));

        let r2 = read_jsonl_line(&mut reader, &mut line).unwrap();
        assert!(matches!(r2, JsonlLineState::Complete(8)));

        let r3 = read_jsonl_line(&mut reader, &mut line).unwrap();
        assert!(matches!(r3, JsonlLineState::Eof));
    }

    #[test]
    fn test_read_jsonl_line_complete_then_partial() {
        let data = b"{\"a\":1}\n{\"b\":2}";
        let mut reader = std::io::BufReader::new(&data[..]);
        let mut line = String::new();

        let r1 = read_jsonl_line(&mut reader, &mut line).unwrap();
        assert!(matches!(r1, JsonlLineState::Complete(8)));

        let r2 = read_jsonl_line(&mut reader, &mut line).unwrap();
        assert!(matches!(r2, JsonlLineState::Partial));
    }

    #[test]
    fn test_read_jsonl_line_discards_oversized_record_and_reads_next_line() {
        let oversized_bytes = MAX_JSONL_EVENT_BYTES + 1;
        let mut data = vec![b'x'; oversized_bytes];
        data.push(b'\n');
        data.extend_from_slice(b"{\"kept\":true}\n");
        let mut reader = std::io::BufReader::with_capacity(31, data.as_slice());
        let mut line = String::new();

        let first = read_jsonl_line(&mut reader, &mut line).unwrap();
        assert!(matches!(
            first,
            JsonlLineState::Oversized(bytes) if bytes == oversized_bytes + 1
        ));
        assert!(line.is_empty());
        assert!(line.capacity() <= MAX_JSONL_EVENT_BYTES);

        let second = read_jsonl_line(&mut reader, &mut line).unwrap();
        assert!(matches!(second, JsonlLineState::Complete(14)));
        assert_eq!(line, "{\"kept\":true}\n");
    }

    #[test]
    fn test_read_jsonl_line_does_not_advance_incomplete_oversized_record() {
        let data = vec![b'x'; MAX_JSONL_EVENT_BYTES + 1];
        let mut reader = std::io::BufReader::with_capacity(31, data.as_slice());
        let mut line = String::new();

        let result = read_jsonl_line(&mut reader, &mut line).unwrap();
        assert!(matches!(result, JsonlLineState::Partial));
        assert!(line.is_empty());
        assert!(line.capacity() <= MAX_JSONL_EVENT_BYTES);
    }

    #[test]
    fn test_find_in_jsonl_lines_skips_oversized_record() {
        let mut data = vec![b'x'; MAX_JSONL_EVENT_BYTES + 1];
        data.extend_from_slice(b"\n{\"cwd\":\"/repo\"}\n");
        let mut reader = std::io::BufReader::with_capacity(31, data.as_slice());

        let cwd = find_in_jsonl_lines(&mut reader, 2, |line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get("cwd")?
                .as_str()
                .map(str::to_owned)
        });

        assert_eq!(cwd.as_deref(), Some("/repo"));
    }
}
