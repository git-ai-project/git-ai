use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{TimestampWatermark, WatermarkStrategy};
use chrono::DateTime;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct KiloAgent {
    batch_size: usize,
}

impl KiloAgent {
    pub fn new() -> Self {
        Self { batch_size: 1000 }
    }

    #[cfg(test)]
    pub fn with_batch_size(batch_size: usize) -> Self {
        Self { batch_size }
    }

    fn open_sqlite_readonly(path: &Path) -> Result<Connection, TranscriptError> {
        let conn =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| {
                TranscriptError::Fatal {
                    message: format!(
                        "Failed to open Kilo database {}: {}",
                        path.display(),
                        e
                    ),
                }
            })?;

        conn.execute_batch("PRAGMA cache_size = -2000;")
            .map_err(|e| TranscriptError::Fatal {
                message: format!("Failed to set PRAGMA cache_size: {}", e),
            })?;

        Ok(conn)
    }

    fn read_session_messages_raw_with_limit(
        conn: &Connection,
        session_id: &str,
        after_updated: i64,
        limit: usize,
    ) -> Result<Vec<(String, i64, serde_json::Value)>, TranscriptError> {
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, time_created, time_updated, data FROM message \
                 WHERE session_id = ? AND time_updated > ? \
                 ORDER BY time_updated ASC, id ASC \
                 LIMIT ?",
            )
            .map_err(|e| TranscriptError::Fatal {
                message: format!("Failed to prepare message query: {}", e),
            })?;

        let rows = stmt
            .query_map(rusqlite::params![session_id, after_updated, limit], |row| {
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
            let (id, row_session_id, _time_created, time_updated, data) =
                row.map_err(|e| TranscriptError::Fatal {
                    message: format!("Failed to read message row: {}", e),
                })?;

            let parsed_data: serde_json::Value =
                serde_json::from_str(&data).map_err(|e| TranscriptError::Parse {
                    line: 0,
                    message: format!("Failed to parse message data for id {}: {}", id, e),
                })?;

            let mut map = serde_json::Map::with_capacity(5);
            map.insert("id".into(), serde_json::Value::String(id.clone()));
            map.insert(
                "session_id".into(),
                serde_json::Value::String(row_session_id),
            );
            map.insert("data".into(), parsed_data);

            messages.push((id, time_updated, serde_json::Value::Object(map)));
        }

        Ok(messages)
    }

    fn read_parts_for_messages_with_limit(
        conn: &Connection,
        session_id: &str,
        after_updated: i64,
        limit: usize,
    ) -> Result<HashMap<String, Vec<serde_json::Value>>, TranscriptError> {
        let mut stmt = conn
            .prepare(
                "SELECT id, message_id, session_id, time_created, time_updated, data FROM part \
                 WHERE message_id IN ( \
                     SELECT id FROM message WHERE session_id = ? AND time_updated > ? ORDER BY time_updated ASC, id ASC LIMIT ? \
                 ) \
                 ORDER BY message_id ASC, time_updated ASC, id ASC",
            )
            .map_err(|e| TranscriptError::Fatal {
                message: format!("Failed to prepare part query: {}", e),
            })?;

        let rows = stmt
            .query_map(rusqlite::params![session_id, after_updated, limit], |row| {
                let id: String = row.get(0)?;
                let message_id: String = row.get(1)?;
                let row_session_id: String = row.get(2)?;
                let time_created: i64 = row.get(3)?;
                let time_updated: i64 = row.get(4)?;
                let data: String = row.get(5)?;
                Ok((
                    id,
                    message_id,
                    row_session_id,
                    time_created,
                    time_updated,
                    data,
                ))
            })
            .map_err(|e| TranscriptError::Fatal {
                message: format!("Failed to query parts: {}", e),
            })?;

        let mut parts_by_message: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
        for row in rows {
            let (_id, message_id, _row_session_id, _time_created, _time_updated, data) =
                row.map_err(|e| TranscriptError::Fatal {
                    message: format!("Failed to read part row: {}", e),
                })?;

            if let Ok(parsed_data) = serde_json::from_str::<serde_json::Value>(&data) {
                let mut map = serde_json::Map::with_capacity(6);
                map.insert("data".into(), parsed_data);
                parts_by_message
                    .entry(message_id)
                    .or_default()
                    .push(serde_json::Value::Object(map));
            }
        }

        Ok(parts_by_message)
    }

    fn kilocode_data_path() -> Option<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            let home = std::env::var("HOME").ok()?;
            Some(PathBuf::from(home).join(".local").join("share").join("kilo"))
        }

        #[cfg(target_os = "linux")]
        {
            if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
                Some(PathBuf::from(xdg_data).join("kilo"))
            } else {
                let home = std::env::var("HOME").ok()?;
                Some(PathBuf::from(home).join(".local").join("share").join("kilo"))
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(app_data) = std::env::var("APPDATA") {
                Some(PathBuf::from(app_data).join("kilo"))
            } else if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
                Some(PathBuf::from(local_app_data).join("kilo"))
            } else {
                None
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            None
        }
    }

    fn resolve_sqlite_db_path(path: &Path) -> Option<PathBuf> {
        if path.is_file() {
            return path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| *name == "kilo.db")
                .map(|_| path.to_path_buf());
        }

        if !path.is_dir() {
            return None;
        }

        let direct_db = path.join("kilo.db");
        if direct_db.exists() {
            return Some(direct_db);
        }

        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "storage")
        {
            let sibling_db = path.parent()?.join("kilo.db");
            if sibling_db.exists() {
                return Some(sibling_db);
            }
        }

        None
    }

    fn discover_sessions_from_db(
        db_path: &Path,
    ) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        let conn = Self::open_sqlite_readonly(db_path)?;

        let mut stmt = conn
            .prepare("SELECT id FROM session")
            .map_err(|e| TranscriptError::Fatal {
                message: format!("Failed to prepare session query: {}", e),
            })?;

        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| TranscriptError::Fatal {
                message: format!("Failed to query sessions: {}", e),
            })?;

        let mut sessions = Vec::new();
        for row in rows {
            let session_id = row.map_err(|e| TranscriptError::Fatal {
                message: format!("Failed to read session row: {}", e),
            })?;

            let parent_id: Option<String> = conn
                .query_row(
                    "SELECT parent_id FROM session WHERE id = ?",
                    [&session_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten();

            sessions.push(DiscoveredSession {
                session_id: session_id.clone(),
                tool: "kilo".to_string(),
                transcript_path: db_path.to_path_buf(),
                transcript_format: crate::transcripts::sweep::TranscriptFormat::KiloCodeSqlite,
                watermark_type: crate::transcripts::watermark::WatermarkType::Timestamp,
                initial_watermark: Box::new(TimestampWatermark::new(DateTime::UNIX_EPOCH)),
                external_session_id: session_id,
                external_parent_session_id: parent_id,
            });
        }

        Ok(sessions)
    }
}

impl Default for KiloAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent for KiloAgent {
    fn batch_size_hint(&self) -> usize {
        self.batch_size
    }

    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        let kilocode_path = if let Ok(test_path) = std::env::var("GIT_AI_KILO_STORAGE_PATH") {
            PathBuf::from(test_path)
        } else {
            Self::kilocode_data_path().ok_or_else(|| TranscriptError::Fatal {
                message: "Could not determine Kilo Code data path".to_string(),
            })?
        };

        let db_path = Self::resolve_sqlite_db_path(&kilocode_path).ok_or_else(|| {
            TranscriptError::Fatal {
                message: format!("Could not find kilo.db at {}", kilocode_path.display()),
            }
        })?;

        Self::discover_sessions_from_db(&db_path)
    }

    fn read_incremental(
        &self,
        path: &Path,
        watermark: Box<dyn WatermarkStrategy>,
        session_id: &str,
    ) -> Result<TranscriptBatch, TranscriptError> {
        let ts_watermark = watermark
            .as_any()
            .downcast_ref::<TimestampWatermark>()
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "Kilo reader requires TimestampWatermark, got incompatible type for session {}",
                    session_id
                ),
            })?;

        let watermark_millis = ts_watermark.0.timestamp_millis();

        let conn = Self::open_sqlite_readonly(path)?;

        let messages = Self::read_session_messages_raw_with_limit(
            &conn,
            session_id,
            watermark_millis,
            self.batch_size,
        )?;

        if messages.is_empty() {
            return Ok(TranscriptBatch {
                events: Vec::new(),
                new_watermark: Box::new(TimestampWatermark::new(ts_watermark.0)),
            });
        }

        let mut parts_by_message = Self::read_parts_for_messages_with_limit(
            &conn,
            session_id,
            watermark_millis,
            self.batch_size,
        )?;

        let mut max_updated: i64 = watermark_millis;
        let mut events = Vec::with_capacity(messages.len());

        for (msg_id, time_updated, msg_data) in messages {
            if time_updated > max_updated {
                max_updated = time_updated;
            }

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
                        .and_then(|v| v.as_str())
                        .map(String::from)
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
        crate::daemon::transcript_worker::extract_event_timestamp(event).unwrap_or_else(|| {
            crate::transcripts::agent::file_time_fallback(file_meta, is_first_event)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sweep_strategy() {
        let agent = KiloAgent::new();
        assert_eq!(
            agent.sweep_strategy(),
            SweepStrategy::Periodic(Duration::from_secs(30 * 60))
        );
    }

    #[test]
    fn test_resolve_sqlite_db_path_direct_file() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("kilo.db");
        std::fs::write(&db_path, b"dummy").unwrap();
        assert_eq!(
            KiloAgent::resolve_sqlite_db_path(&db_path),
            Some(db_path)
        );
    }

    #[test]
    fn test_resolve_sqlite_db_path_other_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("not_kilo.db");
        std::fs::write(&db_path, b"dummy").unwrap();
        assert_eq!(KiloAgent::resolve_sqlite_db_path(&db_path), None);
    }

    #[test]
    fn test_resolve_sqlite_db_path_directory_with_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("kilo.db");
        std::fs::write(&db_path, b"dummy").unwrap();
        assert_eq!(
            KiloAgent::resolve_sqlite_db_path(dir.path()),
            Some(db_path)
        );
    }

    #[test]
    fn test_resolve_sqlite_db_path_directory_without_db() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(KiloAgent::resolve_sqlite_db_path(dir.path()), None);
    }

    #[test]
    fn test_resolve_sqlite_db_path_storage_dir_with_sibling_db() {
        let dir = tempfile::tempdir().unwrap();
        let storage_dir = dir.path().join("storage");
        std::fs::create_dir(&storage_dir).unwrap();
        let db_path = dir.path().join("kilo.db");
        std::fs::write(&db_path, b"dummy").unwrap();
        assert_eq!(
            KiloAgent::resolve_sqlite_db_path(&storage_dir),
            Some(db_path)
        );
    }

    #[test]
    fn test_discover_sessions_empty_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("kilo.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT);",
        )
        .unwrap();
        drop(conn);

        let sessions = KiloAgent::discover_sessions_from_db(&db_path).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_discover_sessions_with_data() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("kilo.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT);
             INSERT INTO session VALUES ('sess-1', NULL);
             INSERT INTO session VALUES ('sess-2', 'sess-1');
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);",
        ).unwrap();
        drop(conn);

        let sessions = KiloAgent::discover_sessions_from_db(&db_path).unwrap();
        assert_eq!(sessions.len(), 2);

        let sess1 = sessions
            .iter()
            .find(|s| s.external_session_id == "sess-1")
            .unwrap();
        assert_eq!(sess1.tool, "kilo");
        assert_eq!(sess1.external_parent_session_id, None);

        let sess2 = sessions
            .iter()
            .find(|s| s.external_session_id == "sess-2")
            .unwrap();
        assert_eq!(sess2.external_parent_session_id, Some("sess-1".to_string()));
    }

    fn create_test_db(path: &std::path::Path, message_count: usize) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS session (id TEXT PRIMARY KEY, parent_id TEXT);
             INSERT OR IGNORE INTO session VALUES ('test-session', NULL);
             CREATE TABLE IF NOT EXISTS message (
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
        agent: &KiloAgent,
        path: &std::path::Path,
    ) -> (Vec<serde_json::Value>, Box<dyn WatermarkStrategy>) {
        let mut all = Vec::new();
        let mut wm: Box<dyn WatermarkStrategy> =
            Box::new(TimestampWatermark::new(DateTime::<chrono::Utc>::UNIX_EPOCH));
        loop {
            let batch = agent.read_incremental(path, wm, "test-session").unwrap();
            if batch.events.is_empty() {
                wm = batch.new_watermark;
                break;
            }
            all.extend(batch.events);
            wm = batch.new_watermark;
        }
        (all, wm)
    }

    #[test]
    fn test_batch_resume_no_loss_or_repeat() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("kilo.db");
        create_test_db(&db_path, 5);

        let agent = KiloAgent::with_batch_size(2);
        let (events, _) = drain_all(&agent, &db_path);

        assert_eq!(events.len(), 5);
        let ids: Vec<u64> = events
            .iter()
            .map(|e| e["message"]["data"]["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn test_append_one_record_after_full_read() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("kilo.db");
        create_test_db(&db_path, 3);

        let agent = KiloAgent::with_batch_size(2);
        let (_, wm) = drain_all(&agent, &db_path);

        let conn = rusqlite::Connection::open(&db_path).unwrap();
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
    fn test_limit_caps_memory_and_watermark_still_drains_all() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("kilo.db");
        create_test_db(&db_path, 20);

        let agent = KiloAgent::with_batch_size(3);
        let (events, _) = drain_all(&agent, &db_path);

        assert_eq!(events.len(), 20);
        let ids: Vec<u64> = events
            .iter()
            .map(|e| e["message"]["data"]["id"].as_u64().unwrap())
            .collect();
        let expected: Vec<u64> = (0..20).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn test_extract_event_ids_with_tool_call() {
        let agent = KiloAgent::new();
        let event = serde_json::json!({
            "message": {
                "id": "msg_123",
                "session_id": "ses_456",
                "time_created": 1000,
                "time_updated": 2000,
                "data": {
                    "role": "assistant",
                    "parentID": "msg_parent",
                    "modelID": "some-model"
                }
            },
            "parts": [
                {
                    "id": "prt_789",
                    "message_id": "msg_123",
                    "session_id": "ses_456",
                    "time_created": 1000,
                    "time_updated": 2000,
                    "data": {
                        "type": "tool",
                        "callID": "call_tool_1",
                        "tool": "read"
                    }
                }
            ]
        });
        let (eid, pid, tid) = agent.extract_event_ids(&event);
        assert_eq!(eid, Some("msg_123".to_string()));
        assert_eq!(pid, Some("msg_parent".to_string()));
        assert_eq!(tid, Some("call_tool_1".to_string()));
    }

    #[test]
    fn test_extract_event_ids_no_parts() {
        let agent = KiloAgent::new();
        let event = serde_json::json!({
            "message": {
                "id": "msg_123",
                "session_id": "ses_456",
                "time_created": 1000,
                "time_updated": 1000,
                "data": {"role": "user"}
            }
        });
        let (eid, pid, tid) = agent.extract_event_ids(&event);
        assert_eq!(eid, Some("msg_123".to_string()));
        assert_eq!(pid, None);
        assert_eq!(tid, None);
    }

    #[test]
    fn test_kilocode_data_path_linux_no_xdg() {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let path = KiloAgent::kilocode_data_path();
            assert!(path.is_some());
        }
    }
}
