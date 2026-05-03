//! Gemini agent implementation with sweep discovery.

use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy, TranscriptFormat};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{TimestampWatermark, WatermarkStrategy, WatermarkType};
use chrono::{DateTime, Utc};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Gemini agent that discovers conversations from Gemini session storage.
pub struct GeminiAgent;

impl GeminiAgent {
    /// Scan for Gemini session files in standard locations.
    ///
    /// Searches `~/.gemini/sessions/` recursively for `*.json` files.
    fn scan_session_files() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        if let Some(sessions_dir) = dirs::home_dir().map(|p| p.join(".gemini/sessions"))
            && sessions_dir.exists()
        {
            Self::scan_json_recursive(&sessions_dir, &mut paths);
        }

        paths
    }

    /// Recursively scan directory for `*.json` files.
    fn scan_json_recursive(dir: &Path, paths: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::scan_json_recursive(&path, paths);
            } else if path.is_file() && path.extension().map(|ext| ext == "json").unwrap_or(false) {
                paths.push(path);
            }
        }
    }

    /// Extract session ID from a Gemini session file path.
    ///
    /// Session ID format: `gemini:{file_stem}`
    fn extract_session_id(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| format!("gemini:{}", s))
    }
}

impl Agent for GeminiAgent {
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

            let session = DiscoveredSession {
                session_id,
                agent_type: "gemini".to_string(),
                transcript_path: path,
                transcript_format: TranscriptFormat::GeminiJson,
                watermark_type: WatermarkType::Timestamp,
                initial_watermark: Box::new(TimestampWatermark::new(DateTime::<Utc>::UNIX_EPOCH)),
                model: None,
                tool: Some("Gemini".to_string()),
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
        // Downcast watermark to TimestampWatermark
        let ts_watermark = watermark
            .as_any()
            .downcast_ref::<TimestampWatermark>()
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "Gemini reader requires TimestampWatermark, got incompatible type for session {}",
                    session_id
                ),
            })?;

        let watermark_timestamp = ts_watermark.0;

        let file = fs::File::open(path).map_err(|e| {
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
                    retry_after: Duration::from_secs(5),
                }
            }
        })?;

        let reader = std::io::BufReader::new(file);
        let mut parsed: serde_json::Value =
            serde_json::from_reader(reader).map_err(|e| TranscriptError::Parse {
                line: 0,
                message: format!("Invalid JSON in {}: {}", path.display(), e),
            })?;

        let messages = match parsed
            .as_object_mut()
            .and_then(|obj| obj.remove("messages"))
        {
            Some(serde_json::Value::Array(arr)) => arr,
            _ => {
                return Err(TranscriptError::Fatal {
                    message: format!(
                        "Missing 'messages' array in Gemini session file: {}",
                        path.display()
                    ),
                });
            }
        };

        let batch_limit = self.batch_size_hint();
        let mut events = Vec::with_capacity(batch_limit);
        let mut max_timestamp = watermark_timestamp;

        for message in messages {
            let parsed_dt = message
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));

            if let Some(dt) = parsed_dt {
                if dt <= watermark_timestamp {
                    continue;
                }
                if dt > max_timestamp {
                    max_timestamp = dt;
                }
            }

            events.push(message);
            if events.len() >= batch_limit {
                break;
            }
        }

        let new_watermark = Box::new(TimestampWatermark::new(max_timestamp));

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
        let agent = GeminiAgent;
        assert_eq!(
            agent.sweep_strategy(),
            SweepStrategy::Periodic(Duration::from_secs(30 * 60))
        );
    }

    #[test]
    fn test_read_incremental_basic() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = serde_json::json!({
            "messages": [
                {"type": "user", "content": "Hello", "timestamp": "2025-01-01T00:00:00Z"},
                {"type": "gemini", "content": "Hi there", "model": "gemini-pro", "timestamp": "2025-01-01T00:00:01Z"}
            ]
        });

        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", json).unwrap();
        file.flush().unwrap();

        let agent = GeminiAgent;
        let watermark = Box::new(TimestampWatermark::new(DateTime::<Utc>::UNIX_EPOCH));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        assert_eq!(result.events.len(), 2);
        // First event is the raw user message
        assert_eq!(result.events[0]["type"], "user");
        assert_eq!(result.events[0]["content"], "Hello");
        // Second event is the raw gemini message
        assert_eq!(result.events[1]["type"], "gemini");
        assert_eq!(result.events[1]["content"], "Hi there");
    }

    #[test]
    fn test_read_incremental_filters_by_watermark() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = serde_json::json!({
            "messages": [
                {"type": "user", "content": "Old message", "timestamp": "2025-01-01T00:00:00Z"},
                {"type": "gemini", "content": "New message", "timestamp": "2025-01-01T00:01:00Z"}
            ]
        });

        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", json).unwrap();
        file.flush().unwrap();

        let agent = GeminiAgent;
        // Set watermark to after the first message
        let ts = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let watermark = Box::new(TimestampWatermark::new(ts));
        let result = agent
            .read_incremental(file.path(), watermark, "test")
            .unwrap();

        // Only the second message should be returned (strictly greater than watermark)
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0]["content"], "New message");
    }
}
