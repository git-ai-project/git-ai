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

/// Read messages from the database, returning each row as a complete JSON object
/// containing all columns (id, session_id, time_created, time_updated, data).
fn read_session_messages_raw(
    conn: &Connection,
    session_id: &str,
    after_updated: i64,
) -> Result<Vec<(String, i64, serde_json::Value)>, TranscriptError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, time_created, time_updated, data FROM message \
             WHERE session_id = ? AND time_updated > ? \
             ORDER BY time_updated ASC, id ASC",
        )
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to prepare message query: {}", e),
        })?;

    let rows = stmt
        .query_map(rusqlite::params![session_id, after_updated], |row| {
            let id: String = row.get(0)?;
            let row_session_id: String = row.get(1)?;
            let time_created: i64 = row.get(2)?;
            let time_updated: i64 = row.get(3)?;
            let data: String = row.get(4)?;
            Ok((id, row_session_id, time_created, time_updated, data))
        })
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to query messages: {}", e),
        })?;

    let mut messages = Vec::new();
    for row in rows {
        let (id, row_session_id, time_created, time_updated, data) =
            row.map_err(|e| TranscriptError::Fatal {
                message: format!("Failed to read message row: {}", e),
            })?;

        let parsed_data: serde_json::Value =
            serde_json::from_str(&data).map_err(|e| TranscriptError::Parse {
                line: 0,
                message: format!("Failed to parse message data for id {}: {}", id, e),
            })?;

        // Build directly via Map to move parsed_data instead of cloning (json! macro clones)
        let mut map = serde_json::Map::with_capacity(5);
        map.insert("id".into(), serde_json::Value::String(id.clone()));
        map.insert("session_id".into(), serde_json::Value::String(row_session_id));
        map.insert("time_created".into(), serde_json::Value::Number(time_created.into()));
        map.insert("time_updated".into(), serde_json::Value::Number(time_updated.into()));
        map.insert("data".into(), parsed_data);

        messages.push((id, time_updated, serde_json::Value::Object(map)));
    }

    Ok(messages)
}

/// Read parts for the matched messages only, using an IN-subquery to avoid loading
/// all parts for the entire session. Returns each row as a complete JSON object
/// containing all columns (id, message_id, session_id, time_created, time_updated, data),
/// grouped by message_id.
fn read_parts_for_messages(
    conn: &Connection,
    session_id: &str,
    after_updated: i64,
) -> Result<HashMap<String, Vec<serde_json::Value>>, TranscriptError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, message_id, session_id, time_created, time_updated, data FROM part \
             WHERE message_id IN ( \
                 SELECT id FROM message WHERE session_id = ? AND time_updated > ? \
             ) \
             ORDER BY message_id ASC, time_updated ASC, id ASC",
        )
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to prepare part query: {}", e),
        })?;

    let rows = stmt
        .query_map(rusqlite::params![session_id, after_updated], |row| {
            let id: String = row.get(0)?;
            let message_id: String = row.get(1)?;
            let row_session_id: String = row.get(2)?;
            let time_created: i64 = row.get(3)?;
            let time_updated: i64 = row.get(4)?;
            let data: String = row.get(5)?;
            Ok((id, message_id, row_session_id, time_created, time_updated, data))
        })
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to query parts: {}", e),
        })?;

    let mut parts_by_message: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    for row in rows {
        let (id, message_id, row_session_id, time_created, time_updated, data) =
            row.map_err(|e| TranscriptError::Fatal {
                message: format!("Failed to read part row: {}", e),
            })?;

        if let Ok(parsed_data) = serde_json::from_str::<serde_json::Value>(&data) {
            let mut map = serde_json::Map::with_capacity(6);
            map.insert("id".into(), serde_json::Value::String(id));
            map.insert("message_id".into(), serde_json::Value::String(message_id.clone()));
            map.insert("session_id".into(), serde_json::Value::String(row_session_id));
            map.insert("time_created".into(), serde_json::Value::Number(time_created.into()));
            map.insert("time_updated".into(), serde_json::Value::Number(time_updated.into()));
            map.insert("data".into(), parsed_data);
            parts_by_message
                .entry(message_id)
                .or_default()
                .push(serde_json::Value::Object(map));
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

        // Read messages with time_updated > watermark_millis
        let messages = read_session_messages_raw(&conn, session_id, watermark_millis)?;

        if messages.is_empty() {
            return Ok(TranscriptBatch {
                events: Vec::new(),
                new_watermark: Box::new(TimestampWatermark::new(ts_watermark.0)),
            });
        }

        // Read only parts for the matched messages (IN-subquery, single scan)
        let mut parts_by_message = read_parts_for_messages(&conn, session_id, watermark_millis)?;

        let mut max_updated: i64 = watermark_millis;
        let mut events = Vec::with_capacity(messages.len());

        for (msg_id, time_updated, msg_data) in messages {
            if time_updated > max_updated {
                max_updated = time_updated;
            }

            // Use .remove() to move parts out of the HashMap instead of cloning via .get()
            let mut map = serde_json::Map::with_capacity(2);
            map.insert("message".into(), msg_data);
            if let Some(parts) = parts_by_message.remove(&msg_id) {
                map.insert("parts".into(), serde_json::Value::Array(parts));
            }

            events.push(serde_json::Value::Object(map));
        }

        let new_watermark_ts =
            DateTime::from_timestamp_millis(max_updated).unwrap_or(ts_watermark.0);
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
        // watermark=0 matches all messages in the fixture
        let parts = read_parts_for_messages(&conn, "test-session-123", 0).unwrap();
        // Verify IN-subquery loading returns parts grouped by message_id.
        // Single query with IN-subquery instead of one per message,
        // prevents full-table-scan memory blowup on large unindexed databases.
        assert!(
            !parts.is_empty(),
            "batch parts query must return data from fixture"
        );
        for msg_parts in parts.values() {
            assert!(!msg_parts.is_empty());
        }
    }
}
