//! Core types for transcript processing.

use std::time::Duration;

/// Errors that can occur during transcript processing.
#[derive(Debug, Clone)]
pub enum TranscriptError {
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

impl std::fmt::Display for TranscriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranscriptError::Transient {
                message,
                retry_after,
            } => write!(
                f,
                "Transient error (retry after {:?}): {}",
                retry_after, message
            ),
            TranscriptError::Parse { line, message } => {
                write!(f, "Parse error at line {}: {}", line, message)
            }
            TranscriptError::Fatal { message } => write!(f, "Fatal error: {}", message),
        }
    }
}

impl std::error::Error for TranscriptError {}

/// Batch of transcript events returned by transcript readers after processing.
pub struct TranscriptBatch {
    /// Raw JSON events from the transcript.
    pub events: Vec<serde_json::Value>,
    /// Updated watermark position after processing this batch.
    pub new_watermark: Box<dyn crate::transcripts::WatermarkStrategy>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transient_error_display() {
        let err = TranscriptError::Transient {
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
        let err = TranscriptError::Parse {
            line: 42,
            message: "invalid JSON".to_string(),
        };
        let display = format!("{}", err);
        assert!(display.contains("Parse error at line 42"));
        assert!(display.contains("invalid JSON"));
    }

    #[test]
    fn test_fatal_error_display() {
        let err = TranscriptError::Fatal {
            message: "file deleted".to_string(),
        };
        let display = format!("{}", err);
        assert!(display.contains("Fatal error"));
        assert!(display.contains("file deleted"));
    }

    #[test]
    fn test_error_is_std_error() {
        let err = TranscriptError::Fatal {
            message: "test".to_string(),
        };
        let _: &dyn std::error::Error = &err;
    }

    #[test]
    fn test_error_clone() {
        let err = TranscriptError::Transient {
            message: "test".to_string(),
            retry_after: Duration::from_secs(10),
        };
        let cloned = err.clone();
        match cloned {
            TranscriptError::Transient {
                message,
                retry_after,
            } => {
                assert_eq!(message, "test");
                assert_eq!(retry_after, Duration::from_secs(10));
            }
            _ => panic!("Expected Transient variant"),
        }
    }
}
