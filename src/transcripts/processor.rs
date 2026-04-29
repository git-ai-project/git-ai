//! Transcript processor dispatcher that routes to format-specific readers.

use super::types::{TranscriptBatch, TranscriptError};
use super::watermark::WatermarkStrategy;
use std::path::Path;

/// Supported transcript formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    ClaudeJsonl,
    CursorJsonl,
    DroidJsonl,
    CopilotSessionJson,
    CopilotEventStreamJsonl,
}

impl TranscriptFormat {
    /// Get the watermark type that should be used for this format.
    pub fn default_watermark_type(&self) -> super::watermark::WatermarkType {
        use super::watermark::WatermarkType;
        match self {
            TranscriptFormat::ClaudeJsonl => WatermarkType::ByteOffset,
            TranscriptFormat::CursorJsonl => WatermarkType::ByteOffset,
            TranscriptFormat::DroidJsonl => WatermarkType::Hybrid,
            TranscriptFormat::CopilotSessionJson => WatermarkType::ByteOffset,
            TranscriptFormat::CopilotEventStreamJsonl => WatermarkType::ByteOffset,
        }
    }
}

/// Process a transcript file from a given watermark position.
///
/// Dispatches to the appropriate format-specific reader based on the format.
///
/// # Arguments
///
/// * `format` - The transcript format to parse
/// * `path` - Path to the transcript file
/// * `watermark` - Current watermark position to resume from
/// * `session_id` - Session ID for context (used in event attributes)
///
/// # Returns
///
/// A `TranscriptBatch` containing:
/// - Parsed events as `AgentTraceValues`
/// - Optional model information extracted from transcript
/// - Updated watermark position
///
/// # Errors
///
/// Returns `TranscriptError` for:
/// - `Transient`: File locked, temporary I/O errors
/// - `Parse`: Malformed JSON, unexpected format
/// - `Fatal`: File not found, permissions denied
pub fn process_transcript(
    format: TranscriptFormat,
    path: &Path,
    watermark: Box<dyn WatermarkStrategy>,
    session_id: &str,
) -> Result<TranscriptBatch, TranscriptError> {
    match format {
        TranscriptFormat::ClaudeJsonl => {
            super::formats::claude::read_incremental(path, watermark, session_id)
        }
        TranscriptFormat::CursorJsonl => {
            // TODO(Phase 2b): Implement Cursor reader
            Err(TranscriptError::Fatal {
                message: "Cursor format not yet implemented".to_string(),
            })
        }
        TranscriptFormat::DroidJsonl => {
            // TODO(Phase 2b): Implement Droid reader
            Err(TranscriptError::Fatal {
                message: "Droid format not yet implemented".to_string(),
            })
        }
        TranscriptFormat::CopilotSessionJson => {
            // TODO(Phase 2b): Implement Copilot session reader
            Err(TranscriptError::Fatal {
                message: "Copilot session format not yet implemented".to_string(),
            })
        }
        TranscriptFormat::CopilotEventStreamJsonl => {
            // TODO(Phase 2b): Implement Copilot event stream reader
            Err(TranscriptError::Fatal {
                message: "Copilot event stream format not yet implemented".to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_default_watermark_types() {
        use super::super::watermark::WatermarkType;

        assert_eq!(
            TranscriptFormat::ClaudeJsonl.default_watermark_type(),
            WatermarkType::ByteOffset
        );
        assert_eq!(
            TranscriptFormat::CursorJsonl.default_watermark_type(),
            WatermarkType::ByteOffset
        );
        assert_eq!(
            TranscriptFormat::DroidJsonl.default_watermark_type(),
            WatermarkType::Hybrid
        );
    }

    #[test]
    fn test_format_equality() {
        assert_eq!(TranscriptFormat::ClaudeJsonl, TranscriptFormat::ClaudeJsonl);
        assert_ne!(TranscriptFormat::ClaudeJsonl, TranscriptFormat::CursorJsonl);
    }
}
