// src/transcripts/model_extraction.rs

use super::sweep::TranscriptFormat;
use super::types::TranscriptError;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

/// Extract model name from the last message in a transcript.
///
/// Reads from the end of the file backwards to avoid loading the entire transcript.
/// Returns None if model cannot be determined.
pub fn extract_model_from_tail(
    path: &Path,
    format: TranscriptFormat,
) -> Result<Option<String>, TranscriptError> {
    match format {
        TranscriptFormat::ClaudeJsonl => extract_model_from_jsonl_tail(path, "model"),
        TranscriptFormat::CursorJsonl => extract_model_from_jsonl_tail(path, "model"),
        TranscriptFormat::DroidJsonl => extract_model_from_jsonl_tail(path, "model"),
        TranscriptFormat::CopilotEventStreamJsonl => extract_model_from_jsonl_tail(path, "model"),
        TranscriptFormat::CopilotSessionJson => extract_model_from_session_json(path),
    }
}

fn extract_model_from_jsonl_tail(
    path: &Path,
    model_field: &str,
) -> Result<Option<String>, TranscriptError> {
    let mut file = File::open(path).map_err(|e| TranscriptError::Fatal {
        message: format!("failed to open transcript: {}", e),
    })?;

    let file_size = file
        .metadata()
        .map_err(|e| TranscriptError::Fatal {
            message: format!("failed to get file metadata: {}", e),
        })?
        .len();

    if file_size == 0 {
        return Ok(None);
    }

    // Read last 4KB (should be enough for most messages)
    let read_size = std::cmp::min(4096, file_size);
    let seek_pos = file_size - read_size;

    file.seek(SeekFrom::Start(seek_pos))
        .map_err(|e| TranscriptError::Transient {
            message: format!("failed to seek: {}", e),
            retry_after: std::time::Duration::from_secs(5),
        })?;

    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();

    // Parse last complete line
    if let Some(last_line) = lines.last() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(last_line) {
            // Try to find model in various locations
            if let Some(model) = json.get(model_field).and_then(|v| v.as_str()) {
                return Ok(Some(model.to_string()));
            }
            // Try nested in message.model
            if let Some(message) = json.get("message") {
                if let Some(model) = message.get(model_field).and_then(|v| v.as_str()) {
                    return Ok(Some(model.to_string()));
                }
            }
        }
    }

    Ok(None)
}

fn extract_model_from_session_json(path: &Path) -> Result<Option<String>, TranscriptError> {
    // For session.json formats, model might be in metadata at top of file
    let file = File::open(path).map_err(|e| TranscriptError::Fatal {
        message: format!("failed to open transcript: {}", e),
    })?;

    let json: serde_json::Value = serde_json::from_reader(file).map_err(|e| {
        TranscriptError::Parse {
            line: 0,
            message: format!("failed to parse session.json: {}", e),
        }
    })?;

    // Try common locations for model field
    if let Some(model) = json.get("model").and_then(|v| v.as_str()) {
        return Ok(Some(model.to_string()));
    }
    if let Some(metadata) = json.get("metadata") {
        if let Some(model) = metadata.get("model").and_then(|v| v.as_str()) {
            return Ok(Some(model.to_string()));
        }
    }

    Ok(None)
}
