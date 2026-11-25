//! Core types for transcript processing.

use std::fs::File;
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

/// Result of reading a single line from a JSONL reader.
pub enum JsonlLineState {
    /// End of file reached.
    Eof,
    /// Incomplete line (no trailing newline) — writer still appending.
    Partial,
    /// Complete line ready for processing. Contains bytes read.
    Complete(usize),
}

/// Read a line from a BufReader, detecting partial writes from concurrent writers.
///
/// Returns `Eof` if no more data, `Partial` if the line lacks a trailing newline,
/// or `Complete(bytes)` on success.
pub fn read_jsonl_line(
    reader: &mut impl BufRead,
    line: &mut String,
) -> std::io::Result<JsonlLineState> {
    line.clear();
    let bytes_read = reader.read_line(line)?;
    if bytes_read == 0 {
        return Ok(JsonlLineState::Eof);
    }
    if !line.ends_with('\n') {
        return Ok(JsonlLineState::Partial);
    }
    Ok(JsonlLineState::Complete(bytes_read))
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

/// Search whole records newest-first within a bounded tail window. The
/// second result tells the caller whether the window left out older records.
pub(super) fn find_in_jsonl_tail<T>(
    path: &Path,
    scan_bytes: u64,
    extract_from_line: fn(&str) -> Option<T>,
) -> (Option<T>, bool) {
    let Ok(mut file) = File::open(path) else {
        return (None, false);
    };
    let Ok(metadata) = file.metadata() else {
        return (None, false);
    };
    let file_size = metadata.len();
    if file_size == 0 {
        return (None, false);
    }

    let window_start = file_size.saturating_sub(scan_bytes);
    let read_start = window_start.saturating_sub(1);
    if file.seek(SeekFrom::Start(read_start)).is_err() {
        return (None, false);
    }

    let tail_was_truncated = window_start > 0;
    let read_size = file_size - read_start;
    let mut tail = Vec::with_capacity(read_size as usize);
    if file.take(read_size).read_to_end(&mut tail).is_err() {
        return (None, tail_was_truncated);
    }

    let mut lines = tail.rsplit(|&byte| byte == b'\n');
    if tail_was_truncated {
        // The extra byte lets us keep the first complete record when the
        // window starts right after a newline and discard a partial one.
        lines.next_back();
    }
    let result = lines.find_map(|line| std::str::from_utf8(line).ok().and_then(extract_from_line));
    (result, tail_was_truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_in_jsonl_tail_keeps_record_at_window_start() {
        let transcript = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(transcript.path(), "\"earlier\"\n\"latest\"\n").unwrap();
        let extract = |line: &str| serde_json::from_str::<String>(line).ok();

        assert_eq!(
            find_in_jsonl_tail(transcript.path(), 9, extract),
            (Some("latest".to_string()), true)
        );
        assert_eq!(
            find_in_jsonl_tail(transcript.path(), 8, extract),
            (None, true)
        );
    }

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
}
