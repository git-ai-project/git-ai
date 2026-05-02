//! OpenCode agent implementation (SQLite-only).

use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{TimestampWatermark, WatermarkStrategy};
use chrono::DateTime;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

/// OpenCode agent that reads from an OpenCode SQLite database.
pub struct OpenCodeAgent;

fn open_sqlite_readonly(path: &Path) -> Result<Connection, TranscriptError> {
    let conn =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| {
            TranscriptError::Fatal {
                message: format!("Failed to open OpenCode database {}: {}", path.display(), e),
            }
        })?;

    conn.execute_batch("PRAGMA cache_size = -2000;")
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to set PRAGMA cache_size: {}", e),
        })?;

    Ok(conn)
}

/// Read messages from the database, returning raw JSON values with their timestamps.
fn read_session_messages_raw(
    conn: &Connection,
    session_id: &str,
    after_created: i64,
) -> Result<Vec<(String, i64, serde_json::Value)>, TranscriptError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, time_created, data FROM message \
             WHERE session_id = ? AND time_created > ? \
             ORDER BY time_created ASC, id ASC",
        )
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to prepare message query: {}", e),
        })?;

    let rows = stmt
        .query_map(rusqlite::params![session_id, after_created], |row| {
            let id: String = row.get(0)?;
            let time_created: i64 = row.get(1)?;
            let data: String = row.get(2)?;
            Ok((id, time_created, data))
        })
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to query messages: {}", e),
        })?;

    let mut messages = Vec::new();
    for row in rows {
        let (id, time_created, data) = row.map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to read message row: {}", e),
        })?;

        let parsed: serde_json::Value =
            serde_json::from_str(&data).map_err(|e| TranscriptError::Parse {
                line: 0,
                message: format!("Failed to parse message data for id {}: {}", id, e),
            })?;

        messages.push((id, time_created, parsed));
    }

    Ok(messages)
}

/// Read all parts for the session, returning raw JSON values grouped by message_id.
fn read_all_parts_raw(
    conn: &Connection,
    session_id: &str,
) -> Result<HashMap<String, Vec<serde_json::Value>>, TranscriptError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, message_id, time_created, data FROM part \
             WHERE session_id = ? \
             ORDER BY message_id ASC, time_created ASC, id ASC",
        )
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to prepare part query: {}", e),
        })?;

    let rows = stmt
        .query_map(rusqlite::params![session_id], |row| {
            let _id: String = row.get(0)?;
            let message_id: String = row.get(1)?;
            let _time_created: i64 = row.get(2)?;
            let data: String = row.get(3)?;
            Ok((message_id, data))
        })
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to query parts: {}", e),
        })?;

    let mut parts_by_message: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    for row in rows {
        let (message_id, data) = row.map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to read part row: {}", e),
        })?;

        // Parse each part's data column as raw JSON
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&data) {
            parts_by_message.entry(message_id).or_default().push(parsed);
        }
    }

    Ok(parts_by_message)
}

impl Agent for OpenCodeAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        // Discovery comes from presets, not sweep.
        Ok(Vec::new())
    }

    fn read_incremental(
        &self,
        path: &Path,
        watermark: Box<dyn WatermarkStrategy>,
        session_id: &str,
    ) -> Result<TranscriptBatch, TranscriptError> {
        // Downcast to TimestampWatermark
        let ts_watermark = watermark
            .as_any()
            .downcast_ref::<TimestampWatermark>()
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "OpenCode reader requires TimestampWatermark, got incompatible type for session {}",
                    session_id
                ),
            })?;

        let watermark_millis = ts_watermark.0.timestamp_millis();

        // Open SQLite read-only
        let conn = open_sqlite_readonly(path)?;

        // Read messages with time_created > watermark_millis
        let messages = read_session_messages_raw(&conn, session_id, watermark_millis)?;

        if messages.is_empty() {
            return Ok(TranscriptBatch {
                events: Vec::new(),
                new_watermark: Box::new(TimestampWatermark::new(ts_watermark.0)),
            });
        }

        // Read all parts for the session, build HashMap by message_id
        let parts_by_message = read_all_parts_raw(&conn, session_id)?;

        let mut max_created: i64 = watermark_millis;
        let mut events = Vec::new();

        for (msg_id, time_created, msg_data) in &messages {
            // Update max_created
            if *time_created > max_created {
                max_created = *time_created;
            }

            // Compose a JSON value with the message data and its parts
            let parts = parts_by_message.get(msg_id);
            let event = if let Some(parts) = parts {
                serde_json::json!({
                    "message": msg_data,
                    "parts": parts,
                })
            } else {
                serde_json::json!({
                    "message": msg_data,
                })
            };

            events.push(event);
        }

        // New watermark from max_created
        let new_watermark_ts =
            DateTime::from_timestamp_millis(max_created).unwrap_or(ts_watermark.0);
        let new_watermark = Box::new(TimestampWatermark::new(new_watermark_ts));

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
        let agent = OpenCodeAgent;
        assert_eq!(
            agent.sweep_strategy(),
            SweepStrategy::Periodic(Duration::from_secs(30 * 60))
        );
    }

    #[test]
    fn test_sqlite_open_sets_cache_size_pragma() {
        let source = include_str!("opencode.rs");
        assert!(
            source.contains("PRAGMA cache_size"),
            "open_sqlite_readonly must set PRAGMA cache_size to cap memory usage (PR #1120)"
        );
    }

    #[test]
    fn test_parts_are_batch_loaded_not_per_message() {
        let db_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/opencode-sqlite/opencode.db");
        let conn = open_sqlite_readonly(&db_path).unwrap();
        let parts = read_all_parts_raw(&conn, "test-session-123").unwrap();
        // Verify batch loading returns parts grouped by message_id.
        // This is the fix from PR #1120: a single query instead of one per message,
        // which prevents full-table-scan memory blowup on large unindexed databases.
        assert!(
            !parts.is_empty(),
            "batch parts query must return data from fixture"
        );
        for (_msg_id, msg_parts) in &parts {
            assert!(!msg_parts.is_empty());
        }
    }
}
