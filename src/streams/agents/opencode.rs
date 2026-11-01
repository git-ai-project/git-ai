//! OpenCode agent implementation (SQLite-only).

use crate::streams::agent::{Agent, PathResolverKind, StreamDescriptor};
use crate::streams::sweep::{DiscoveredSession, StreamFormat, SweepStrategy};
use crate::streams::types::{StreamBatch, StreamError};
use crate::streams::watermark::{TimestampCursorWatermark, TimestampWatermark, WatermarkStrategy};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

// Bound source JSON plus row overhead, not RSS: parsing expands JSON. A single
// oversized event is returned alone so its message/parts are never truncated.
const BATCH_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
const ROW_OVERHEAD_BYTES: u64 = 256;

/// OpenCode agent that reads from an OpenCode SQLite database.
pub struct OpenCodeAgent {
    batch_size: usize,
}

impl OpenCodeAgent {
    pub fn new() -> Self {
        Self { batch_size: 1000 }
    }

    #[cfg(test)]
    pub fn with_batch_size(batch_size: usize) -> Self {
        Self { batch_size }
    }
}

pub fn open_sqlite_readonly(path: &Path) -> Result<Connection, StreamError> {
    let conn =
        crate::sqlite::open_with_flags_and_memory_limits(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to open OpenCode database {}: {}", path.display(), e),
            })?;

    conn.execute_batch("PRAGMA busy_timeout = 5000;")
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to set PRAGMAs: {}", e),
        })?;

    Ok(conn)
}

/// Upgraded databases can contain both schemas, so select by session rather
/// than by the presence of the V2 tables. Older databases have no session_v2.
pub(crate) fn is_v2_session(conn: &Connection, session_id: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM session_v2 WHERE id = ?1)",
        [session_id],
        |row| row.get(0),
    )
    .unwrap_or(false)
}

/// Sort only IDs. Native OpenCode indexes narrow by session but do not index
/// time_updated, so putting JSON or its length here would touch old payloads.
fn select_message_ids(
    conn: &Connection,
    session_id: &str,
    after_updated: i64,
    after_id: &str,
    limit: usize,
    v2: bool,
) -> Result<Vec<String>, StreamError> {
    let table = if v2 { "session_message" } else { "message" };
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id FROM {table} WHERE session_id = ?1 \
             AND time_updated >= ?2 AND (time_updated > ?2 OR id > ?3) \
             ORDER BY time_updated ASC, id ASC LIMIT ?4"
        ))
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to prepare message selection: {e}"),
        })?;
    stmt.query_map(
        rusqlite::params![session_id, after_updated, after_id, limit],
        |row| row.get(0),
    )
    .and_then(|rows| rows.collect())
    .map_err(|e| StreamError::Fatal {
        message: format!("Failed to select messages: {e}"),
    })
}

fn message_ids_json(ids: &[String]) -> Result<String, StreamError> {
    serde_json::to_string(ids).map_err(|e| StreamError::Fatal {
        message: format!("Failed to encode selected message IDs: {e}"),
    })
}

/// Size only selected rows. SQLite's octet_length reads byte lengths without
/// loading overflow payload pages (unlike length(CAST(data AS BLOB))). One
/// batched statement lazily aggregates indexed parts for each needed message;
/// no outer sort/group may force sizing future messages before yielding a row.
/// All queries share a snapshot, so selected messages cannot disappear mid-read.
fn prepare_message_sizes(
    conn: &Connection,
    v2: bool,
    has_parts: bool,
) -> Result<rusqlite::Statement<'_>, StreamError> {
    let message_table = if v2 { "session_message" } else { "message" };
    let part_table = if v2 { "session_part" } else { "part" };
    let part_sizes = if has_parts {
        format!(
            "+ COALESCE((SELECT SUM(octet_length(p.data) \
             + octet_length(p.message_id) + octet_length(p.session_id) \
             + {ROW_OVERHEAD_BYTES}) FROM {part_table} p \
             WHERE p.message_id = m.id), 0)"
        )
    } else {
        String::new()
    };
    conn.prepare(&format!(
        "SELECT m.id, octet_length(m.data) \
         + octet_length(m.id) + octet_length(m.session_id) \
         + {ROW_OVERHEAD_BYTES} {part_sizes} \
         FROM json_each(?1) selected CROSS JOIN {message_table} m \
         WHERE m.id = selected.value"
    ))
    .map_err(|e| StreamError::Fatal {
        message: format!("Failed to prepare transcript sizes: {e}"),
    })
}

fn bound_message_ids(
    conn: &Connection,
    ids: &mut Vec<String>,
    v2: bool,
    has_parts: bool,
) -> Result<String, StreamError> {
    let mut stmt = prepare_message_sizes(conn, v2, has_parts)?;
    bound_message_ids_from_statement(&mut stmt, ids)
}

fn bound_message_ids_from_statement(
    stmt: &mut rusqlite::Statement<'_>,
    ids: &mut Vec<String>,
) -> Result<String, StreamError> {
    let mut rows = stmt
        .query_map([message_ids_json(ids)?], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to read transcript sizes: {e}"),
        })?;
    let mut sizes = HashMap::new();
    let mut bytes = 0_u64;
    let mut count = 0;
    for id in ids.iter() {
        // The JSON-driven join normally yields IDs in the selected order, but
        // correctness must not depend on SQL row order. Buffer any early rows
        // and advance only the contiguous prefix of the original selection.
        let size = loop {
            if let Some(size) = sizes.remove(id) {
                break size;
            }
            let (row_id, size) = rows
                .next()
                .ok_or_else(|| StreamError::Fatal {
                    message: format!("Selected OpenCode message disappeared: {id}"),
                })?
                .map_err(|e| StreamError::Fatal {
                    message: format!("Failed to read transcript sizes: {e}"),
                })?;
            sizes.insert(row_id, size);
        };
        if count > 0 && bytes.saturating_add(size) > BATCH_SOURCE_BYTES {
            break;
        }
        bytes = bytes.saturating_add(size);
        count += 1;
        if bytes >= BATCH_SOURCE_BYTES {
            break;
        }
    }
    ids.truncate(count);
    message_ids_json(ids)
}

/// Read messages as native database rows, including V2's type and sequence.
fn read_messages(
    conn: &Connection,
    ids_json: &str,
    v2: bool,
) -> Result<Vec<(String, i64, serde_json::Value)>, StreamError> {
    let table = if v2 { "session_message" } else { "message" };
    let extra_columns = if v2 { ", type, seq" } else { "" };
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id, session_id, time_created, time_updated, data{extra_columns} FROM {table} \
             WHERE id IN (SELECT value FROM json_each(?1)) ORDER BY time_updated ASC, id ASC"
        ))
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to prepare message query: {}", e),
        })?;

    let rows = stmt
        .query_map([ids_json], |row| {
            let id: String = row.get(0)?;
            let row_session_id: String = row.get(1)?;
            let time_created: i64 = row.get(2)?;
            let time_updated: i64 = row.get(3)?;
            let data: String = row.get(4)?;
            let v2_fields = if v2 {
                Some((row.get::<_, String>(5)?, row.get::<_, i64>(6)?))
            } else {
                None
            };
            Ok((
                id,
                row_session_id,
                time_created,
                time_updated,
                data,
                v2_fields,
            ))
        })
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to query messages: {}", e),
        })?;

    let mut messages = Vec::new();
    for row in rows {
        let (id, row_session_id, time_created, time_updated, data, v2_fields) =
            row.map_err(|e| StreamError::Fatal {
                message: format!("Failed to read message row: {}", e),
            })?;

        let parsed_data: serde_json::Value =
            serde_json::from_str(&data).map_err(|e| StreamError::Parse {
                line: 0,
                message: format!("Failed to parse message data for id {}: {}", id, e),
            })?;

        // Build directly via Map to move parsed_data instead of cloning (json! macro clones)
        let mut map = serde_json::Map::with_capacity(5);
        map.insert("id".into(), serde_json::Value::String(id.clone()));
        map.insert(
            "session_id".into(),
            serde_json::Value::String(row_session_id),
        );
        map.insert(
            "time_created".into(),
            serde_json::Value::Number(time_created.into()),
        );
        map.insert(
            "time_updated".into(),
            serde_json::Value::Number(time_updated.into()),
        );
        map.insert("data".into(), parsed_data);
        if let Some((message_type, seq)) = v2_fields {
            map.insert("type".into(), serde_json::Value::String(message_type));
            map.insert("seq".into(), serde_json::Value::Number(seq.into()));
        }

        messages.push((id, time_updated, serde_json::Value::Object(map)));
    }

    Ok(messages)
}

/// Fetch parts by the selected IDs without repeating the session scan/sort.
/// Returns native V1 or V2 rows grouped by
/// message_id, preserving V2's ordinal and call_id instead of inventing a part ID.
fn read_parts_for_messages(
    conn: &Connection,
    ids_json: &str,
    v2: bool,
) -> Result<HashMap<String, Vec<serde_json::Value>>, StreamError> {
    let (table, identity, extra_columns, order) = if v2 {
        ("session_part", "ordinal", ", type, call_id", "ordinal ASC")
    } else {
        ("part", "id", "", "time_updated ASC, id ASC")
    };
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {identity}, message_id, session_id, time_created, time_updated, data{extra_columns} FROM {table} \
             WHERE message_id IN (SELECT value FROM json_each(?1)) \
             ORDER BY message_id ASC, {order}",
        ))
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to prepare part query: {}", e),
        })?;

    let rows = stmt
        .query_map([ids_json], |row| {
            let id = if v2 {
                serde_json::Value::Number(row.get::<_, i64>(0)?.into())
            } else {
                serde_json::Value::String(row.get(0)?)
            };
            let message_id: String = row.get(1)?;
            let row_session_id: String = row.get(2)?;
            let time_created: i64 = row.get(3)?;
            let time_updated: i64 = row.get(4)?;
            let data: String = row.get(5)?;
            let v2_fields = if v2 {
                Some((row.get::<_, String>(6)?, row.get::<_, Option<String>>(7)?))
            } else {
                None
            };
            Ok((
                id,
                message_id,
                row_session_id,
                time_created,
                time_updated,
                data,
                v2_fields,
            ))
        })
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to query parts: {}", e),
        })?;

    let mut parts_by_message: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    for row in rows {
        let (id, message_id, row_session_id, time_created, time_updated, data, v2_fields) = row
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to read part row: {}", e),
            })?;

        if let Ok(parsed_data) = serde_json::from_str::<serde_json::Value>(&data) {
            let mut map = serde_json::Map::with_capacity(6);
            map.insert(identity.into(), id);
            map.insert(
                "message_id".into(),
                serde_json::Value::String(message_id.clone()),
            );
            map.insert(
                "session_id".into(),
                serde_json::Value::String(row_session_id),
            );
            map.insert(
                "time_created".into(),
                serde_json::Value::Number(time_created.into()),
            );
            map.insert(
                "time_updated".into(),
                serde_json::Value::Number(time_updated.into()),
            );
            map.insert("data".into(), parsed_data);
            if let Some((part_type, call_id)) = v2_fields {
                map.insert("type".into(), serde_json::Value::String(part_type));
                map.insert(
                    "call_id".into(),
                    call_id
                        .map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null),
                );
            }
            parts_by_message
                .entry(message_id)
                .or_default()
                .push(serde_json::Value::Object(map));
        }
    }

    Ok(parts_by_message)
}

impl Default for OpenCodeAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent for OpenCodeAgent {
    fn batch_size_hint(&self) -> usize {
        self.batch_size
    }

    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, StreamError> {
        // Discovery comes from presets, not sweep.
        Ok(Vec::new())
    }

    fn read_incremental(
        &self,
        path: &Path,
        watermark: Box<dyn WatermarkStrategy>,
        session_id: &str,
    ) -> Result<StreamBatch, StreamError> {
        let mut cursor = if let Some(cursor) = watermark
            .as_any()
            .downcast_ref::<TimestampCursorWatermark>()
        {
            cursor.clone()
        } else if let Some(legacy) = watermark.as_any().downcast_ref::<TimestampWatermark>() {
            // Legacy watermarks did not record the last ID at their timestamp.
            // Replay that boundary once so migration cannot skip tied messages.
            TimestampCursorWatermark::new(legacy.0.timestamp_millis() as f64, String::new())
        } else {
            return Err(StreamError::Fatal {
                message: format!(
                    "OpenCode reader requires TimestampCursorWatermark, got incompatible type for session {}",
                    session_id
                ),
            });
        };

        let watermark_millis = cursor.timestamp_millis as i64;

        // Keep both selectors on the same snapshot: OpenCode can update a
        // message and its parts while this bounded batch is being read.
        let conn = open_sqlite_readonly(path)?;
        let snapshot = conn
            .unchecked_transaction()
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to start OpenCode read snapshot: {}", e),
            })?;
        let v2 = is_v2_session(&snapshot, session_id);

        // The ID breaks timestamp ties without dropping messages at the limit.
        let mut ids = select_message_ids(
            &snapshot,
            session_id,
            watermark_millis,
            &cursor.last_id,
            self.batch_size,
            v2,
        )?;

        if ids.is_empty() {
            return Ok(StreamBatch {
                events: Vec::new(),
                new_watermark: Box::new(cursor),
            });
        }

        // V2.1 embeds content in messages; V2.2 can move it to session_part.
        let has_parts = !v2 || snapshot.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'session_part')",
            [], |row| row.get::<_, bool>(0),
        ).map_err(|e| StreamError::Fatal { message: format!("Failed to detect OpenCode part schema: {e}") })?;
        let ids_json = bound_message_ids(&snapshot, &mut ids, v2, has_parts)?;
        let messages = read_messages(&snapshot, &ids_json, v2)?;
        let mut parts_by_message = if has_parts {
            read_parts_for_messages(&snapshot, &ids_json, v2)?
        } else {
            HashMap::new()
        };
        snapshot.commit().map_err(|e| StreamError::Fatal {
            message: format!("Failed to finish OpenCode read snapshot: {}", e),
        })?;

        let mut events = Vec::with_capacity(messages.len());

        if let Some((id, updated, _)) = messages.last() {
            cursor = TimestampCursorWatermark::new(*updated as f64, id.clone());
        }

        for (msg_id, _, msg_data) in messages {
            // Use .remove() to move parts out of the HashMap instead of cloning via .get()
            let mut map = serde_json::Map::with_capacity(2);
            map.insert("message".into(), msg_data);
            if let Some(parts) = parts_by_message.remove(&msg_id) {
                map.insert("parts".into(), serde_json::Value::Array(parts));
            }

            events.push(serde_json::Value::Object(map));
        }

        Ok(StreamBatch {
            events,
            new_watermark: Box::new(cursor),
        })
    }

    fn extract_event_ids(
        &self,
        event: &serde_json::Value,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let message = event.get("message");

        let event_id = message
            .and_then(|m| m.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let parent_event_id = message
            .and_then(|m| m.get("data"))
            .and_then(|d| d.get("parentID"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let tool_use_id = event
            .get("parts")
            .and_then(|p| p.as_array())
            .and_then(|arr| {
                arr.iter().find_map(|part| {
                    part.get("data")
                        .and_then(|d| d.get("callID"))
                        .or_else(|| part.get("call_id"))
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
            })
            .or_else(|| {
                message?
                    .get("data")?
                    .get("content")?
                    .as_array()?
                    .iter()
                    .find_map(|part| {
                        if part.get("type")?.as_str()? != "tool" {
                            return None;
                        }
                        part.get("id")?.as_str().map(String::from)
                    })
            });

        (event_id, parent_event_id, tool_use_id)
    }

    fn extract_event_timestamp(
        &self,
        event: &serde_json::Value,
        file_meta: &std::fs::Metadata,
        is_first_event: bool,
    ) -> u32 {
        crate::daemon::stream_worker::extract_event_timestamp(event)
            .or_else(|| {
                let message = event.get("message")?;
                message
                    .get("time_created")
                    .and_then(|value| value.as_u64())
                    .or_else(|| {
                        message
                            .pointer("/data/time/created")
                            .and_then(|value| value.as_u64())
                    })
                    .map(|millis| (millis / 1000) as u32)
            })
            .unwrap_or_else(|| crate::streams::agent::file_time_fallback(file_meta, is_first_event))
    }

    fn streams(&self) -> Vec<StreamDescriptor> {
        let format = StreamFormat::OpenCodeSqlite;
        vec![StreamDescriptor {
            stream_kind: "transcript",
            format,
            watermark_type: format.watermark_type(),
            path_resolver: PathResolverKind::Identity,
            shared: false,
            watermark_type_resolver: None,
            format_resolver: None,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sweep_strategy() {
        let agent = OpenCodeAgent::new();
        assert_eq!(
            agent.sweep_strategy(),
            SweepStrategy::Periodic(Duration::from_secs(30 * 60))
        );
    }

    fn create_test_db(path: &std::path::Path, message_count: usize) {
        let conn = crate::sqlite::open_with_memory_limits(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );",
        )
        .unwrap();
        for i in 0..message_count {
            let ts = 1000 + (i as i64) * 1000;
            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    format!("msg-{}", i),
                    "test-session",
                    ts,
                    ts,
                    format!(r#"{{"role":"user","id":{}}}"#, i),
                ],
            ).unwrap();
            conn.execute(
                "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    format!("prt-{}", i),
                    format!("msg-{}", i),
                    "test-session",
                    ts + 1,
                    ts + 1,
                    format!(r#"{{"type":"text","text":"part-{}"}}"#, i),
                ],
            ).unwrap();
        }
    }

    fn drain_all(
        agent: &OpenCodeAgent,
        path: &std::path::Path,
    ) -> (Vec<serde_json::Value>, Box<dyn WatermarkStrategy>) {
        use chrono::{DateTime, Utc};
        let mut all = Vec::new();
        let mut wm: Box<dyn WatermarkStrategy> =
            Box::new(TimestampWatermark::new(DateTime::<Utc>::UNIX_EPOCH));
        loop {
            let batch = agent.read_incremental(path, wm, "test-session").unwrap();
            // Exercise the same serialization/deserialization boundary as the
            // worker when it saves a batch and later resumes the stream.
            wm = agent.streams()[0]
                .watermark_type
                .deserialize(&batch.new_watermark.serialize())
                .unwrap();
            if batch.events.is_empty() {
                break;
            }
            all.extend(batch.events);
        }
        (all, wm)
    }

    #[test]
    fn test_batch_resume_no_loss_or_repeat() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path, 5);

        let agent = OpenCodeAgent::with_batch_size(2);
        let (events, _) = drain_all(&agent, &db_path);

        assert_eq!(events.len(), 5);
        let ids: Vec<u64> = events
            .iter()
            .map(|e| e["message"]["data"]["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn test_opencode_batches_preserve_equal_timestamp_messages_and_parts() {
        for v2 in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let db_path = dir.path().join("test.db");
            create_test_db(&db_path, 5);
            let conn = crate::sqlite::open_with_memory_limits(&db_path).unwrap();
            conn.execute("UPDATE message SET time_updated = 1000", [])
                .unwrap();
            if v2 {
                conn.execute_batch(
                    "CREATE TABLE session_v2 (id TEXT PRIMARY KEY);
                     INSERT INTO session_v2 VALUES ('test-session');
                     ALTER TABLE message RENAME TO session_message;
                     ALTER TABLE session_message ADD COLUMN type TEXT NOT NULL DEFAULT 'user';
                     ALTER TABLE session_message ADD COLUMN seq INTEGER NOT NULL DEFAULT 0;
                     ALTER TABLE part RENAME TO session_part;
                     ALTER TABLE session_part ADD COLUMN ordinal INTEGER NOT NULL DEFAULT 0;
                     ALTER TABLE session_part ADD COLUMN type TEXT NOT NULL DEFAULT 'text';
                     ALTER TABLE session_part ADD COLUMN call_id TEXT;",
                )
                .unwrap();
            }
            drop(conn);

            let agent = OpenCodeAgent::with_batch_size(2);
            let (events, watermark) = drain_all(&agent, &db_path);
            let ids: Vec<_> = events
                .iter()
                .map(|event| event["message"]["id"].as_str().unwrap())
                .collect();
            assert_eq!(ids, ["msg-0", "msg-1", "msg-2", "msg-3", "msg-4"]);
            for (index, event) in events.iter().enumerate() {
                assert_eq!(event["parts"].as_array().unwrap().len(), 1);
                assert_eq!(event["parts"][0]["message_id"], event["message"]["id"]);
                assert_eq!(event["parts"][0]["data"]["text"], format!("part-{index}"));
            }
            assert!(
                agent
                    .read_incremental(&db_path, watermark, "test-session")
                    .unwrap()
                    .events
                    .is_empty()
            );
        }
    }

    #[test]
    fn test_opencode_legacy_timestamp_replays_boundary_once() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path, 3);
        let agent = OpenCodeAgent::with_batch_size(1);
        let legacy =
            TimestampWatermark::new(chrono::DateTime::from_timestamp_millis(2000).unwrap());
        let first = agent
            .read_incremental(&db_path, Box::new(legacy), "test-session")
            .unwrap();
        assert_eq!(first.events[0]["message"]["id"], "msg-1");
        let resumed = agent.streams()[0]
            .watermark_type
            .deserialize(&first.new_watermark.serialize())
            .unwrap();
        let second = agent
            .read_incremental(&db_path, resumed, "test-session")
            .unwrap();
        assert_eq!(second.events[0]["message"]["id"], "msg-2");
        assert!(
            agent
                .read_incremental(&db_path, second.new_watermark, "test-session")
                .unwrap()
                .events
                .is_empty()
        );
    }

    #[test]
    fn test_opencode_resume_observes_updated_and_deleted_messages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        create_test_db(&path, 5);
        let agent = OpenCodeAgent::with_batch_size(2);
        let first = agent
            .read_incremental(
                &path,
                Box::new(TimestampCursorWatermark::initial()),
                "test-session",
            )
            .unwrap();
        assert_eq!(first.events.len(), 2);
        let conn = crate::sqlite::open_with_memory_limits(&path).unwrap();
        conn.execute("DELETE FROM message WHERE id = 'msg-2'", [])
            .unwrap();
        conn.execute(
            "UPDATE message SET time_updated = 6000 WHERE id = 'msg-0'",
            [],
        )
        .unwrap();
        // An orphaned old part cannot leak into another selected message.
        drop(conn);
        let second = agent
            .read_incremental(&path, first.new_watermark, "test-session")
            .unwrap();
        assert_eq!(
            second
                .events
                .iter()
                .map(|event| event["message"]["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["msg-3", "msg-4"]
        );
        let third = agent
            .read_incremental(&path, second.new_watermark, "test-session")
            .unwrap();
        assert_eq!(third.events.len(), 1);
        assert_eq!(third.events[0]["message"]["id"], "msg-0");
        for event in second.events.iter().chain(&third.events) {
            assert_eq!(event["parts"].as_array().unwrap().len(), 1);
            assert_eq!(event["parts"][0]["message_id"], event["message"]["id"]);
        }
        assert!(
            agent
                .read_incremental(&path, third.new_watermark, "test-session")
                .unwrap()
                .events
                .is_empty()
        );
    }

    #[test]
    fn test_append_one_record_after_full_read() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path, 3);

        let agent = OpenCodeAgent::with_batch_size(2);
        let (all, wm) = drain_all(&agent, &db_path);
        assert_eq!(all.len(), 3);

        // Insert one more record with a later timestamp
        let conn = crate::sqlite::open_with_memory_limits(&db_path).unwrap();
        let ts = 1000 + 3 * 1000i64;
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["msg-3", "test-session", ts, ts, r#"{"role":"user","id":3}"#],
        ).unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params!["prt-3", "msg-3", "test-session", ts+1, ts+1, r#"{"type":"text","text":"part-3"}"#],
        ).unwrap();
        drop(conn);

        let batch = agent
            .read_incremental(&db_path, wm, "test-session")
            .unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(
            batch.events[0]["message"]["data"]["id"].as_u64().unwrap(),
            3
        );
    }

    #[test]
    fn test_append_several_records_after_full_read() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path, 3);

        let agent = OpenCodeAgent::with_batch_size(2);
        let (_, mut wm) = drain_all(&agent, &db_path);

        // Insert 3 more records
        let conn = crate::sqlite::open_with_memory_limits(&db_path).unwrap();
        for i in 3..6usize {
            let ts = 1000 + (i as i64) * 1000;
            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    format!("msg-{}", i),
                    "test-session",
                    ts, ts,
                    format!(r#"{{"role":"user","id":{}}}"#, i),
                ],
            ).unwrap();
            conn.execute(
                "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    format!("prt-{}", i),
                    format!("msg-{}", i),
                    "test-session",
                    ts+1, ts+1,
                    format!(r#"{{"type":"text","text":"part-{}"}}"#, i),
                ],
            ).unwrap();
        }
        drop(conn);

        let mut new_events = Vec::new();
        loop {
            let batch = agent
                .read_incremental(&db_path, wm, "test-session")
                .unwrap();
            wm = batch.new_watermark;
            if batch.events.is_empty() {
                break;
            }
            new_events.extend(batch.events);
        }
        assert_eq!(new_events.len(), 3);
        let ids: Vec<u64> = new_events
            .iter()
            .map(|e| e["message"]["data"]["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![3, 4, 5]);
    }

    #[test]
    fn test_sqlite_open_sets_cache_size_pragma() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("opencode.db");
        drop(crate::sqlite::open_with_memory_limits(&db_path).unwrap());

        let conn = open_sqlite_readonly(&db_path).unwrap();

        let cache_size: i32 = conn
            .pragma_query_value(None, "cache_size", |row| row.get(0))
            .unwrap();
        assert_eq!(cache_size, crate::sqlite::MEMORY_LIMIT_CACHE_SIZE_KIB);
    }

    #[test]
    fn test_limit_caps_memory_and_watermark_still_drains_all() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path, 20);

        // batch_size=3 forces multiple iterations to drain 20 messages
        let agent = OpenCodeAgent::with_batch_size(3);
        let (events, _) = drain_all(&agent, &db_path);

        assert_eq!(
            events.len(),
            20,
            "all 20 messages must be returned across batches"
        );
        let ids: Vec<u64> = events
            .iter()
            .map(|e| e["message"]["data"]["id"].as_u64().unwrap())
            .collect();
        let expected: Vec<u64> = (0..20).collect();
        assert_eq!(
            ids, expected,
            "messages must arrive in order with no gaps or duplicates"
        );
    }

    #[test]
    fn test_limit_returns_at_most_batch_size_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path, 10);

        let agent = OpenCodeAgent::with_batch_size(4);
        let wm: Box<dyn WatermarkStrategy> = Box::new(TimestampWatermark::new(
            chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        ));

        let batch = agent
            .read_incremental(&db_path, wm, "test-session")
            .unwrap();
        assert!(
            batch.events.len() <= 4,
            "single call must not exceed batch_size (got {})",
            batch.events.len()
        );
    }

    /// Manual before/after RSS and catch-up probe. Run through `task test` with
    /// TEST_FILTER=opencode_reader_stress_probe EXTRA_TEST_BINARY_ARGS=--ignored.
    #[test]
    #[ignore = "manual transcript memory and query scaling probe"]
    fn test_opencode_reader_stress_probe() {
        let setting = |name: &str, default: usize| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(default)
        };
        let count = setting("GIT_AI_OPENCODE_PROBE_MESSAGES", 1000);
        let payload_bytes = setting("GIT_AI_OPENCODE_PROBE_PAYLOAD_BYTES", 64 * 1024);
        let passes = setting("GIT_AI_OPENCODE_PROBE_PASSES", 5);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let mut conn = crate::sqlite::open_with_memory_limits(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_v2 (id TEXT PRIMARY KEY);
             INSERT INTO session_v2 VALUES ('probe');
             CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                type TEXT NOT NULL, seq INTEGER NOT NULL, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE UNIQUE INDEX session_message_session_seq_idx ON session_message(session_id,seq);
             CREATE INDEX session_message_session_type_seq_idx ON session_message(session_id,type,seq);
             CREATE INDEX session_message_session_time_created_id_idx ON session_message(session_id,time_created,id);
             CREATE TABLE session_part (message_id TEXT NOT NULL, session_id TEXT NOT NULL,
                ordinal INTEGER NOT NULL, type TEXT NOT NULL, call_id TEXT,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL,
                PRIMARY KEY(message_id,ordinal));"
        ).unwrap();
        let payload = serde_json::json!({"padding": "x".repeat(payload_bytes)}).to_string();
        let tx = conn.transaction().unwrap();
        for index in 0..count {
            let id = format!("msg-{index:08}");
            tx.execute(
                "INSERT INTO session_message VALUES(?1,'probe','assistant',?2,1000,1000,?3)",
                rusqlite::params![id, index, payload],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO session_part VALUES(?1,'probe',0,'text',NULL,1000,1000,?2)",
                rusqlite::params![id, payload],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        drop(conn);
        drop(payload);
        eprintln!(
            "OPENCODE_PROBE_READY pid={} messages={count} payload_bytes={payload_bytes}",
            std::process::id()
        );
        let reader = OpenCodeAgent::new();
        for pass in 0..passes {
            let start = std::time::Instant::now();
            let mut watermark: Box<dyn WatermarkStrategy> =
                Box::new(TimestampCursorWatermark::initial());
            let mut seen = std::collections::BTreeSet::new();
            let mut max_batch_bytes = 0;
            let mut batches = 0;
            loop {
                let batch = reader.read_incremental(&path, watermark, "probe").unwrap();
                watermark = batch.new_watermark;
                if batch.events.is_empty() {
                    break;
                }
                batches += 1;
                max_batch_bytes = max_batch_bytes.max(
                    batch
                        .events
                        .iter()
                        .map(|event| serde_json::to_vec(event).unwrap().len())
                        .sum::<usize>(),
                );
                for event in batch.events {
                    assert!(seen.insert(event["message"]["id"].as_str().unwrap().to_owned()));
                    assert_eq!(
                        event["message"]["data"]["padding"].as_str().unwrap().len(),
                        payload_bytes
                    );
                    assert_eq!(
                        event["parts"][0]["data"]["padding"].as_str().unwrap().len(),
                        payload_bytes
                    );
                }
            }
            assert_eq!(seen.len(), count);
            eprintln!(
                "OPENCODE_PROBE_PASS pass={pass} events={} batches={batches} max_batch_bytes={max_batch_bytes} elapsed_ms={}",
                seen.len(),
                start.elapsed().as_millis()
            );
        }
    }

    #[test]
    fn test_parts_are_batch_loaded_not_per_message() {
        let db_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/opencode-sqlite/opencode.db");
        let conn = open_sqlite_readonly(&db_path).unwrap();
        let ids = select_message_ids(&conn, "test-session-123", 0, "", 1000, false).unwrap();
        let parts =
            read_parts_for_messages(&conn, &message_ids_json(&ids).unwrap(), false).unwrap();
        // A single batched query fetches only these messages' parts.
        assert!(
            !parts.is_empty(),
            "batch parts query must return data from fixture"
        );
        for msg_parts in parts.values() {
            assert!(!msg_parts.is_empty());
        }
    }

    #[test]
    fn test_opencode_size_planning_stops_after_full_batch() {
        for v2 in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("opencode.db");
            create_test_db(&path, 128);
            let conn = crate::sqlite::open_with_memory_limits(&path).unwrap();
            // Native OpenCode indexes parts by message. Future messages have
            // many parts, but the first oversized event already fills a batch.
            conn.execute_batch(
                "CREATE INDEX part_message ON part(message_id);
                 WITH RECURSIVE ord(n) AS (
                     VALUES(1) UNION ALL SELECT n + 1 FROM ord WHERE n < 63
                 )
                 INSERT INTO part
                 SELECT 'extra-' || m.id || '-' || ord.n, m.id, m.session_id,
                        m.time_created, m.time_updated, '{}'
                 FROM message m CROSS JOIN ord;",
            )
            .unwrap();
            conn.execute(
                "UPDATE message SET data = ?1 WHERE id = 'msg-0'",
                [
                    serde_json::json!({ "padding": "x".repeat(BATCH_SOURCE_BYTES as usize) })
                        .to_string(),
                ],
            )
            .unwrap();
            if v2 {
                conn.execute_batch(
                    "ALTER TABLE message RENAME TO session_message;
                     ALTER TABLE part RENAME TO session_part;",
                )
                .unwrap();
            }

            let mut ids = select_message_ids(&conn, "test-session", 0, "", 1000, v2).unwrap();
            assert_eq!(ids.len(), 128);
            let mut stmt = prepare_message_sizes(&conn, v2, true).unwrap();
            let encoded = bound_message_ids_from_statement(&mut stmt, &mut ids).unwrap();
            assert_eq!(ids, ["msg-0"]);
            assert_eq!(encoded, r#"["msg-0"]"#);
            let steps = stmt.get_status(rusqlite::StatementStatus::VmStep);
            assert!(
                steps < 10_000,
                "sizing a full batch must not aggregate future messages' parts: {steps} VM steps"
            );
        }
    }

    #[test]
    fn test_opencode_size_planning_preserves_prefix_with_reordered_rows() {
        let conn = crate::sqlite::open_with_memory_limits(":memory:").unwrap();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT value, CASE WHEN value = 'third' THEN {} ELSE {} END
                 FROM json_each(?1) ORDER BY key DESC",
                BATCH_SOURCE_BYTES * 2,
                BATCH_SOURCE_BYTES / 2,
            ))
            .unwrap();
        let mut ids = vec!["first".into(), "second".into(), "third".into()];
        bound_message_ids_from_statement(&mut stmt, &mut ids).unwrap();
        assert_eq!(ids, ["first", "second"]);
    }

    #[test]
    fn test_extract_event_ids_with_tool_call() {
        let agent = OpenCodeAgent::new();
        let event = serde_json::json!({
            "message": {
                "id": "msg_c5d3ff79b001I77d7ERgcEhCCc",
                "session_id": "ses_3a2c00870ffebuyMGjJ2UiakYv",
                "time_created": 1000,
                "time_updated": 2000,
                "data": {
                    "role": "assistant",
                    "parentID": "msg_c5d3ff791001Egl5tW62x4Vgzo",
                    "modelID": "big-pickle"
                }
            },
            "parts": [
                {
                    "id": "prt_c5d4001ea001t4tNa4ACM94hno",
                    "message_id": "msg_c5d3ff79b001I77d7ERgcEhCCc",
                    "session_id": "ses_3a2c00870ffebuyMGjJ2UiakYv",
                    "time_created": 1000,
                    "time_updated": 2000,
                    "data": {
                        "type": "tool",
                        "callID": "call_function_p43u37xcf94i_1",
                        "tool": "read",
                        "state": {"status": "completed"}
                    }
                }
            ]
        });
        let (eid, pid, tid) = agent.extract_event_ids(&event);
        assert_eq!(eid, Some("msg_c5d3ff79b001I77d7ERgcEhCCc".to_string()));
        assert_eq!(pid, Some("msg_c5d3ff791001Egl5tW62x4Vgzo".to_string()));
        assert_eq!(tid, Some("call_function_p43u37xcf94i_1".to_string()));
    }

    #[test]
    fn test_extract_event_ids_no_parts() {
        let agent = OpenCodeAgent::new();
        let event = serde_json::json!({
            "message": {
                "id": "msg_c5d3ff791001Egl5tW62x4Vgzo",
                "session_id": "ses_3a2c00870ffebuyMGjJ2UiakYv",
                "time_created": 1000,
                "time_updated": 1000,
                "data": {"role": "user"}
            }
        });
        let (eid, pid, tid) = agent.extract_event_ids(&event);
        assert_eq!(eid, Some("msg_c5d3ff791001Egl5tW62x4Vgzo".to_string()));
        assert_eq!(pid, None);
        assert_eq!(tid, None);
    }

    #[test]
    fn test_extract_event_ids_with_parent_no_tool() {
        let agent = OpenCodeAgent::new();
        let event = serde_json::json!({
            "message": {
                "id": "msg_c5d400371001TvbvIzWZB1f9il",
                "session_id": "ses_3a2c00870ffebuyMGjJ2UiakYv",
                "time_created": 1000,
                "time_updated": 2000,
                "data": {
                    "role": "assistant",
                    "parentID": "msg_c5d3ff791001Egl5tW62x4Vgzo",
                    "modelID": "big-pickle"
                }
            },
            "parts": [
                {
                    "id": "prt_c5d4002f20016aBCkx6UdvIDBo",
                    "message_id": "msg_c5d400371001TvbvIzWZB1f9il",
                    "session_id": "ses_3a2c00870ffebuyMGjJ2UiakYv",
                    "time_created": 1000,
                    "time_updated": 2000,
                    "data": {
                        "type": "step-finish",
                        "reason": "tool-calls",
                        "cost": 0
                    }
                }
            ]
        });
        let (eid, pid, tid) = agent.extract_event_ids(&event);
        assert_eq!(eid, Some("msg_c5d400371001TvbvIzWZB1f9il".to_string()));
        assert_eq!(pid, Some("msg_c5d3ff791001Egl5tW62x4Vgzo".to_string()));
        assert_eq!(tid, None);
    }
}
