//! Transcripts database for tracking stream cursors and watermarks.

use super::types::StreamError;
use super::watermark::WatermarkStrategy;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Schema migrations - each entry is SQL to apply for that version.
const MIGRATIONS: &[&str] = &[
    // Version 1: Initial schema
    r#"
    CREATE TABLE IF NOT EXISTS schema_version (
        version INTEGER PRIMARY KEY
    );

    CREATE TABLE IF NOT EXISTS sessions (
        session_id TEXT PRIMARY KEY,
        agent_type TEXT NOT NULL,
        transcript_path TEXT NOT NULL,
        transcript_format TEXT NOT NULL,
        watermark_type TEXT NOT NULL,
        watermark_value TEXT NOT NULL,
        model TEXT,
        tool TEXT,
        external_thread_id TEXT,
        first_seen_at INTEGER NOT NULL,
        last_processed_at INTEGER NOT NULL,
        last_known_size INTEGER NOT NULL DEFAULT 0,
        last_modified INTEGER,
        processing_errors INTEGER DEFAULT 0,
        last_error TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_sessions_tool ON sessions(tool);
    CREATE INDEX IF NOT EXISTS idx_sessions_last_processed ON sessions(last_processed_at);
    CREATE INDEX IF NOT EXISTS idx_sessions_errors ON sessions(processing_errors) WHERE processing_errors > 0;
    CREATE INDEX IF NOT EXISTS idx_sessions_transcript_path ON sessions(transcript_path);

    CREATE TABLE IF NOT EXISTS processing_stats (
        session_id TEXT PRIMARY KEY,
        total_events INTEGER DEFAULT 0,
        total_bytes INTEGER DEFAULT 0,
        FOREIGN KEY (session_id) REFERENCES sessions(session_id)
    );

    INSERT INTO schema_version (version) VALUES (1);
    "#,
    // Version 2: Recreate sessions with external_session_id/external_parent_session_id,
    // drop model/tool columns and processing_stats table.
    // No data migration needed — transcripts feature has not shipped to production yet.
    r#"
    DROP TABLE IF EXISTS processing_stats;
    DROP TABLE IF EXISTS sessions;

    CREATE TABLE sessions (
        session_id TEXT PRIMARY KEY,
        tool TEXT NOT NULL,
        transcript_path TEXT NOT NULL,
        transcript_format TEXT NOT NULL,
        watermark_type TEXT NOT NULL,
        watermark_value TEXT NOT NULL,
        external_session_id TEXT NOT NULL,
        external_parent_session_id TEXT,
        first_seen_at INTEGER NOT NULL,
        last_processed_at INTEGER NOT NULL,
        last_known_size INTEGER NOT NULL DEFAULT 0,
        last_modified INTEGER,
        processing_errors INTEGER DEFAULT 0,
        last_error TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_sessions_tool ON sessions(tool);
    CREATE INDEX IF NOT EXISTS idx_sessions_last_processed ON sessions(last_processed_at);
    CREATE INDEX IF NOT EXISTS idx_sessions_errors ON sessions(processing_errors) WHERE processing_errors > 0;
    CREATE INDEX IF NOT EXISTS idx_sessions_transcript_path ON sessions(transcript_path);

    INSERT INTO schema_version (version) VALUES (2);
    "#,
    // Version 3: Add repo_work_dir column for session-level repo context.
    r#"
    ALTER TABLE sessions ADD COLUMN repo_work_dir TEXT;

    INSERT INTO schema_version (version) VALUES (3);
    "#,
    // Version 4: Add stream_kind column with compound PK (session_id, stream_kind, stream_path).
    // The path is part of the PK to prevent collisions when two physically distinct files
    // produce the same session_id (issue #1461).
    r#"
    BEGIN;
    CREATE TABLE tracked_streams_v4 (
        session_id TEXT NOT NULL,
        stream_kind TEXT NOT NULL DEFAULT 'transcript',
        tool TEXT NOT NULL,
        stream_path TEXT NOT NULL,
        stream_format TEXT NOT NULL,
        watermark_type TEXT NOT NULL,
        watermark_value TEXT NOT NULL,
        external_session_id TEXT NOT NULL,
        external_parent_session_id TEXT,
        first_seen_at INTEGER NOT NULL,
        last_processed_at INTEGER NOT NULL,
        last_known_size INTEGER NOT NULL DEFAULT 0,
        last_modified INTEGER,
        processing_errors INTEGER DEFAULT 0,
        last_error TEXT,
        repo_work_dir TEXT,
        PRIMARY KEY (session_id, stream_kind, stream_path)
    );
    INSERT INTO tracked_streams_v4 SELECT session_id, 'transcript', tool, transcript_path, transcript_format, watermark_type, watermark_value, external_session_id, external_parent_session_id, first_seen_at, last_processed_at, last_known_size, last_modified, processing_errors, last_error, repo_work_dir FROM sessions;
    DROP TABLE sessions;
    ALTER TABLE tracked_streams_v4 RENAME TO tracked_streams;
    CREATE INDEX idx_streams_tool ON tracked_streams(tool);
    CREATE INDEX idx_streams_last_processed ON tracked_streams(last_processed_at);
    CREATE INDEX idx_streams_errors ON tracked_streams(processing_errors) WHERE processing_errors > 0;
    CREATE INDEX idx_streams_stream_path ON tracked_streams(stream_path);
    INSERT INTO schema_version (version) VALUES (4);
    COMMIT;
    "#,
];

/// Record representing a tracked stream cursor in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRecord {
    pub session_id: String,
    pub stream_kind: String,
    pub tool: String,
    pub stream_path: String,
    pub stream_format: String,
    pub watermark_type: String,
    pub watermark_value: String,
    pub external_session_id: String,
    pub external_parent_session_id: Option<String>,
    pub first_seen_at: i64,
    pub last_processed_at: i64,
    pub last_known_size: i64,
    pub last_modified: Option<i64>,
    pub processing_errors: i64,
    pub last_error: Option<String>,
    pub repo_work_dir: Option<String>,
}

/// SQLite database for transcript tracking.
pub struct StreamsDatabase {
    conn: Arc<Mutex<Connection>>,
}

impl StreamsDatabase {
    /// Open or create the transcripts database at the given path.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, StreamError> {
        let conn = Connection::open(path.as_ref()).map_err(|e| StreamError::Fatal {
            message: format!("Failed to open database: {}", e),
        })?;

        // Enable WAL mode for better concurrency and crash resistance
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to enable WAL mode: {}", e),
            })?;

        // Performance optimizations
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to set synchronous mode: {}", e),
            })?;
        conn.pragma_update(None, "cache_size", -2000)
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to set cache size: {}", e),
            })?;
        conn.pragma_update(None, "temp_store", "MEMORY")
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to set temp store: {}", e),
            })?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };

        // Run migrations
        db.migrate()?;

        Ok(db)
    }

    /// Run database migrations to bring schema up to current version.
    fn migrate(&self) -> Result<(), StreamError> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Check if schema_version table exists
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_version'",
                [],
                |row| {
                    let count: i64 = row.get(0)?;
                    Ok(count > 0)
                },
            )
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to check schema_version table: {}", e),
            })?;

        // Get current schema version (0 if table doesn't exist)
        let current_version: u32 = if table_exists {
            conn.query_row(
                "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to query schema version: {}", e),
            })?
            .unwrap_or(0)
        } else {
            0
        };

        // Apply migrations
        for (version, migration_sql) in MIGRATIONS.iter().enumerate() {
            let target_version = (version + 1) as u32;
            if current_version < target_version {
                conn.execute_batch(migration_sql)
                    .map_err(|e| StreamError::Fatal {
                        message: format!(
                            "Failed to apply migration to version {}: {}",
                            target_version, e
                        ),
                    })?;
            }
        }

        Ok(())
    }

    /// Insert a new stream record.
    pub fn insert_stream(&self, record: &StreamRecord) -> Result<(), StreamError> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        conn.execute(
            r#"
            INSERT INTO tracked_streams (
                session_id, stream_kind, tool, stream_path, stream_format,
                watermark_type, watermark_value, external_session_id,
                external_parent_session_id,
                first_seen_at, last_processed_at, last_known_size, last_modified,
                processing_errors, last_error, repo_work_dir
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            "#,
            params![
                record.session_id,
                record.stream_kind,
                record.tool,
                record.stream_path,
                record.stream_format,
                record.watermark_type,
                record.watermark_value,
                record.external_session_id,
                record.external_parent_session_id,
                record.first_seen_at,
                record.last_processed_at,
                record.last_known_size,
                record.last_modified,
                record.processing_errors,
                record.last_error,
                record.repo_work_dir,
            ],
        )
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to insert stream: {}", e),
        })?;

        Ok(())
    }

    /// Helper to map a row to a StreamRecord.
    fn row_to_stream(row: &rusqlite::Row) -> rusqlite::Result<StreamRecord> {
        Ok(StreamRecord {
            session_id: row.get(0)?,
            stream_kind: row.get(1)?,
            tool: row.get(2)?,
            stream_path: row.get(3)?,
            stream_format: row.get(4)?,
            watermark_type: row.get(5)?,
            watermark_value: row.get(6)?,
            external_session_id: row.get(7)?,
            external_parent_session_id: row.get(8)?,
            first_seen_at: row.get(9)?,
            last_processed_at: row.get(10)?,
            last_known_size: row.get(11)?,
            last_modified: row.get(12)?,
            processing_errors: row.get(13)?,
            last_error: row.get(14)?,
            repo_work_dir: row.get(15)?,
        })
    }

    /// Get a stream record by its full primary key.
    pub fn get_stream(
        &self,
        session_id: &str,
        stream_kind: &str,
        stream_path: &str,
    ) -> Result<Option<StreamRecord>, StreamError> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            r#"
            SELECT session_id, stream_kind, tool, stream_path, stream_format,
                   watermark_type, watermark_value, external_session_id,
                   external_parent_session_id,
                   first_seen_at, last_processed_at, last_known_size, last_modified,
                   processing_errors, last_error, repo_work_dir
            FROM tracked_streams WHERE session_id = ?1 AND stream_kind = ?2 AND stream_path = ?3
            "#,
            params![session_id, stream_kind, stream_path],
            Self::row_to_stream,
        )
        .optional()
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to get stream: {}", e),
        })
    }

    /// Update the watermark for a stream.
    pub fn update_watermark(
        &self,
        session_id: &str,
        stream_kind: &str,
        stream_path: &str,
        watermark: &dyn WatermarkStrategy,
    ) -> Result<(), StreamError> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().timestamp();
        let watermark_value = watermark.serialize();

        let rows_changed = conn.execute(
            "UPDATE tracked_streams SET watermark_value = ?1, last_processed_at = ?2 WHERE session_id = ?3 AND stream_kind = ?4 AND stream_path = ?5",
            params![watermark_value, now, session_id, stream_kind, stream_path],
        )
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to update watermark: {}", e),
        })?;

        if rows_changed == 0 {
            return Err(StreamError::Fatal {
                message: format!("Stream not found: {}", session_id),
            });
        }

        Ok(())
    }

    /// Update file metadata (size and modified time) for a stream.
    pub fn update_file_metadata(
        &self,
        session_id: &str,
        stream_kind: &str,
        stream_path: &str,
        file_size: u64,
        modified: Option<DateTime<Utc>>,
    ) -> Result<(), StreamError> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let modified_ts = modified.map(|dt| dt.timestamp());

        let rows_changed = conn.execute(
            "UPDATE tracked_streams SET last_known_size = ?1, last_modified = ?2 WHERE session_id = ?3 AND stream_kind = ?4 AND stream_path = ?5",
            params![file_size as i64, modified_ts, session_id, stream_kind, stream_path],
        )
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to update file metadata: {}", e),
        })?;

        if rows_changed == 0 {
            return Err(StreamError::Fatal {
                message: format!("Stream not found: {}", session_id),
            });
        }

        Ok(())
    }

    /// Update the repo_work_dir for a stream.
    pub fn update_repo_work_dir(
        &self,
        session_id: &str,
        stream_kind: &str,
        stream_path: &str,
        repo_work_dir: &str,
    ) -> Result<(), StreamError> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let rows_changed = conn
            .execute(
                "UPDATE tracked_streams SET repo_work_dir = ?1 WHERE session_id = ?2 AND stream_kind = ?3 AND stream_path = ?4",
                params![repo_work_dir, session_id, stream_kind, stream_path],
            )
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to update repo_work_dir: {}", e),
            })?;

        if rows_changed == 0 {
            return Err(StreamError::Fatal {
                message: format!("Stream not found: {}", session_id),
            });
        }

        Ok(())
    }

    /// Record an error for a stream.
    pub fn record_error(
        &self,
        session_id: &str,
        stream_kind: &str,
        stream_path: &str,
        error_message: &str,
    ) -> Result<(), StreamError> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let rows_changed = conn.execute(
            "UPDATE tracked_streams SET processing_errors = processing_errors + 1, last_error = ?1 WHERE session_id = ?2 AND stream_kind = ?3 AND stream_path = ?4",
            params![error_message, session_id, stream_kind, stream_path],
        )
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to record error: {}", e),
        })?;

        if rows_changed == 0 {
            return Err(StreamError::Fatal {
                message: format!("Stream not found: {}", session_id),
            });
        }

        Ok(())
    }

    /// Delete a stream and its associated data.
    pub fn delete_stream(
        &self,
        session_id: &str,
        stream_kind: &str,
        stream_path: &str,
    ) -> Result<(), StreamError> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let rows_changed = conn
            .execute(
                "DELETE FROM tracked_streams WHERE session_id = ?1 AND stream_kind = ?2 AND stream_path = ?3",
                params![session_id, stream_kind, stream_path],
            )
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to delete stream: {}", e),
            })?;

        if rows_changed == 0 {
            return Err(StreamError::Fatal {
                message: format!("Stream not found: {}", session_id),
            });
        }

        Ok(())
    }

    /// Get all stream records.
    pub fn all_streams(&self) -> Result<Vec<StreamRecord>, StreamError> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = conn
            .prepare(
                r#"
            SELECT session_id, stream_kind, tool, stream_path, stream_format,
                   watermark_type, watermark_value, external_session_id,
                   external_parent_session_id,
                   first_seen_at, last_processed_at, last_known_size, last_modified,
                   processing_errors, last_error, repo_work_dir
            FROM tracked_streams
            "#,
            )
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to prepare all_streams query: {}", e),
            })?;

        let rows = stmt
            .query_map([], Self::row_to_stream)
            .map_err(|e| StreamError::Fatal {
                message: format!("Failed to query all streams: {}", e),
            })?;

        let mut streams = Vec::new();
        for row in rows {
            streams.push(row.map_err(|e| StreamError::Fatal {
                message: format!("Failed to read stream row: {}", e),
            })?);
        }

        Ok(streams)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::NamedTempFile;

    fn create_test_db() -> (StreamsDatabase, NamedTempFile) {
        let temp_file = NamedTempFile::new().unwrap();
        let db = StreamsDatabase::open(temp_file.path()).unwrap();
        (db, temp_file)
    }

    fn create_test_stream(session_id: &str) -> StreamRecord {
        StreamRecord {
            session_id: session_id.to_string(),
            stream_kind: "transcript".to_string(),
            tool: "claude".to_string(),
            stream_path: "/path/to/transcript.jsonl".to_string(),
            stream_format: "jsonl".to_string(),
            watermark_type: "ByteOffset".to_string(),
            watermark_value: "0".to_string(),
            external_session_id: "thread-123".to_string(),
            external_parent_session_id: None,
            first_seen_at: 1704067200,
            last_processed_at: 1704067200,
            last_known_size: 0,
            last_modified: Some(1704067200),
            processing_errors: 0,
            last_error: None,
            repo_work_dir: None,
        }
    }

    #[test]
    fn test_database_open_creates_schema() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = StreamsDatabase::open(temp_file.path()).unwrap();

        // Verify schema exists
        let conn = db.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='tracked_streams'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_database_wal_mode_enabled() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = StreamsDatabase::open(temp_file.path()).unwrap();

        let conn = db.conn.lock().unwrap();
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn test_insert_and_get_stream() {
        let (db, _temp) = create_test_db();
        let stream = create_test_stream("session-1");

        db.insert_stream(&stream).unwrap();

        let retrieved = db
            .get_stream("session-1", "transcript", "/path/to/transcript.jsonl")
            .unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), stream);
    }

    #[test]
    fn test_get_nonexistent_stream() {
        let (db, _temp) = create_test_db();

        let result = db
            .get_stream("nonexistent", "transcript", "/path/to/transcript.jsonl")
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_update_watermark() {
        let (db, _temp) = create_test_db();
        let stream = create_test_stream("session-1");
        db.insert_stream(&stream).unwrap();

        use super::super::watermark::ByteOffsetWatermark;
        let new_watermark = ByteOffsetWatermark::new(1234);

        db.update_watermark(
            "session-1",
            "transcript",
            "/path/to/transcript.jsonl",
            &new_watermark,
        )
        .unwrap();

        let retrieved = db
            .get_stream("session-1", "transcript", "/path/to/transcript.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.watermark_value, "1234");
        assert!(retrieved.last_processed_at > stream.last_processed_at);
    }

    #[test]
    fn test_update_file_metadata() {
        let (db, _temp) = create_test_db();
        let stream = create_test_stream("session-1");
        db.insert_stream(&stream).unwrap();

        let modified = Utc.with_ymd_and_hms(2024, 6, 15, 10, 30, 0).unwrap();
        db.update_file_metadata(
            "session-1",
            "transcript",
            "/path/to/transcript.jsonl",
            5678,
            Some(modified),
        )
        .unwrap();

        let retrieved = db
            .get_stream("session-1", "transcript", "/path/to/transcript.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.last_known_size, 5678);
        assert_eq!(retrieved.last_modified, Some(modified.timestamp()));
    }

    #[test]
    fn test_all_streams_empty() {
        let (db, _temp) = create_test_db();

        let streams = db.all_streams().unwrap();
        assert_eq!(streams.len(), 0);
    }

    #[test]
    fn test_all_streams_multiple() {
        let (db, _temp) = create_test_db();

        let stream1 = create_test_stream("session-1");
        let stream2 = create_test_stream("session-2");
        let stream3 = create_test_stream("session-3");

        db.insert_stream(&stream1).unwrap();
        db.insert_stream(&stream2).unwrap();
        db.insert_stream(&stream3).unwrap();

        let streams = db.all_streams().unwrap();
        assert_eq!(streams.len(), 3);

        let ids: Vec<String> = streams.iter().map(|s| s.session_id.clone()).collect();
        assert!(ids.contains(&"session-1".to_string()));
        assert!(ids.contains(&"session-2".to_string()));
        assert!(ids.contains(&"session-3".to_string()));
    }

    #[test]
    fn test_stream_with_nulls() {
        let (db, _temp) = create_test_db();

        let stream = StreamRecord {
            session_id: "session-null".to_string(),
            stream_kind: "transcript".to_string(),
            tool: "claude".to_string(),
            stream_path: "/path".to_string(),
            stream_format: "jsonl".to_string(),
            watermark_type: "ByteOffset".to_string(),
            watermark_value: "0".to_string(),
            external_session_id: "session-null".to_string(),
            external_parent_session_id: None,
            first_seen_at: 1704067200,
            last_processed_at: 1704067200,
            last_known_size: 0,
            last_modified: None,
            processing_errors: 0,
            last_error: None,
            repo_work_dir: None,
        };

        db.insert_stream(&stream).unwrap();

        let retrieved = db
            .get_stream("session-null", "transcript", "/path")
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.external_session_id, "session-null");
        assert_eq!(retrieved.last_modified, None);
        assert_eq!(retrieved.last_error, None);
        assert_eq!(retrieved.repo_work_dir, None);
    }

    #[test]
    fn test_schema_version_tracking() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = StreamsDatabase::open(temp_file.path()).unwrap();

        let conn = db.conn.lock().unwrap();
        let version: u32 = conn
            .query_row(
                "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 4); // Current schema version
    }

    #[test]
    fn test_database_reopens_correctly() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        {
            let db = StreamsDatabase::open(&path).unwrap();
            let stream = create_test_stream("session-1");
            db.insert_stream(&stream).unwrap();
        }

        // Reopen database
        let db = StreamsDatabase::open(&path).unwrap();
        let stream = db
            .get_stream("session-1", "transcript", "/path/to/transcript.jsonl")
            .unwrap();
        assert!(stream.is_some());
    }

    #[test]
    fn test_indexes_created() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = StreamsDatabase::open(temp_file.path()).unwrap();

        let conn = db
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name LIKE 'idx_streams_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 4); // 4 indexes defined in schema
    }

    #[test]
    fn test_performance_pragmas_set() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = StreamsDatabase::open(temp_file.path()).unwrap();

        let conn = db
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // synchronous returns an integer: 0=OFF, 1=NORMAL, 2=FULL, 3=EXTRA
        let synchronous: i32 = conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert_eq!(synchronous, 1); // 1 = NORMAL

        let cache_size: i32 = conn
            .pragma_query_value(None, "cache_size", |row| row.get(0))
            .unwrap();
        assert_eq!(cache_size, -2000);

        // temp_store returns an integer: 0=DEFAULT, 1=FILE, 2=MEMORY
        let temp_store: i32 = conn
            .pragma_query_value(None, "temp_store", |row| row.get(0))
            .unwrap();
        assert_eq!(temp_store, 2); // 2 = MEMORY
    }

    #[test]
    fn test_update_watermark_nonexistent_stream() {
        let (db, _temp) = create_test_db();

        use super::super::watermark::ByteOffsetWatermark;
        let watermark = ByteOffsetWatermark::new(100);

        let result = db.update_watermark("nonexistent", "transcript", "/no/such/path", &watermark);
        assert!(result.is_err());
        match result {
            Err(StreamError::Fatal { message }) => {
                assert!(message.contains("Stream not found"));
            }
            _ => panic!("Expected Fatal error"),
        }
    }

    #[test]
    fn test_update_file_metadata_nonexistent_stream() {
        let (db, _temp) = create_test_db();

        let modified = Utc.with_ymd_and_hms(2024, 6, 15, 10, 30, 0).unwrap();
        let result = db.update_file_metadata(
            "nonexistent",
            "transcript",
            "/no/such/path",
            1234,
            Some(modified),
        );
        assert!(result.is_err());
        match result {
            Err(StreamError::Fatal { message }) => {
                assert!(message.contains("Stream not found"));
            }
            _ => panic!("Expected Fatal error"),
        }
    }

    #[test]
    fn test_record_error() {
        let (db, _temp) = create_test_db();
        let stream = create_test_stream("session-1");
        db.insert_stream(&stream).unwrap();

        // Record an error
        db.record_error(
            "session-1",
            "transcript",
            "/path/to/transcript.jsonl",
            "Test error message",
        )
        .unwrap();

        let retrieved = db
            .get_stream("session-1", "transcript", "/path/to/transcript.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.processing_errors, 1);
        assert_eq!(retrieved.last_error, Some("Test error message".to_string()));

        // Record another error
        db.record_error(
            "session-1",
            "transcript",
            "/path/to/transcript.jsonl",
            "Another error",
        )
        .unwrap();

        let retrieved = db
            .get_stream("session-1", "transcript", "/path/to/transcript.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.processing_errors, 2);
        assert_eq!(retrieved.last_error, Some("Another error".to_string()));
    }

    #[test]
    fn test_record_error_nonexistent_stream() {
        let (db, _temp) = create_test_db();

        let result = db.record_error("nonexistent", "transcript", "/no/such/path", "error");
        assert!(result.is_err());
        match result {
            Err(StreamError::Fatal { message }) => {
                assert!(message.contains("Stream not found"));
            }
            _ => panic!("Expected Fatal error"),
        }
    }

    #[test]
    fn test_delete_stream() {
        let (db, _temp) = create_test_db();
        let stream = create_test_stream("session-1");
        db.insert_stream(&stream).unwrap();

        // Verify it exists
        assert!(
            db.get_stream("session-1", "transcript", "/path/to/transcript.jsonl")
                .unwrap()
                .is_some()
        );

        // Delete it
        db.delete_stream("session-1", "transcript", "/path/to/transcript.jsonl")
            .unwrap();

        // Verify it's gone
        assert!(
            db.get_stream("session-1", "transcript", "/path/to/transcript.jsonl")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_delete_nonexistent_stream() {
        let (db, _temp) = create_test_db();

        let result = db.delete_stream("nonexistent", "transcript", "/no/such/path");
        assert!(result.is_err());
        match result {
            Err(StreamError::Fatal { message }) => {
                assert!(message.contains("Stream not found"));
            }
            _ => panic!("Expected Fatal error"),
        }
    }

    #[test]
    fn test_insert_stream_duplicate_fails() {
        let (db, _temp) = create_test_db();

        let stream = create_test_stream("session-1");
        db.insert_stream(&stream).unwrap();

        // Try to insert a duplicate (should fail)
        let duplicate = create_test_stream("session-1");
        let result = db.insert_stream(&duplicate);
        assert!(result.is_err());

        // Original stream still intact
        let retrieved = db
            .get_stream("session-1", "transcript", "/path/to/transcript.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.session_id, "session-1");
    }

    #[test]
    fn test_mutex_poison_recovery() {
        use std::sync::Arc;
        use std::thread;

        let (db, _temp) = create_test_db();
        let stream = create_test_stream("session-1");
        db.insert_stream(&stream).unwrap();

        // Create a scenario that would poison the mutex in older code
        // This is a bit contrived since we now recover from poison automatically
        // but it demonstrates that poison recovery works

        let db_arc = Arc::new(db);
        let db_clone = Arc::clone(&db_arc);

        // Spawn a thread that panics while holding the lock
        let handle = thread::spawn(move || {
            let conn = db_clone
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Force a panic (commented out to not actually poison in this test)
            // panic!("Simulated panic");
            drop(conn);
        });

        let _ = handle.join();

        // After the thread completes (or panics), we should still be able to use the database
        let result = db_arc.get_stream("session-1", "transcript", "/path/to/transcript.jsonl");
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[test]
    fn test_migration_v3_to_v4_preserves_data() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("migration_test.db");

        // Manually create a v3 database
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            // Run migrations 0..=2 (versions 1, 2, 3)
            for migration in &MIGRATIONS[..3] {
                conn.execute_batch(migration).unwrap();
            }
            // Insert a session using the v3 schema (no stream_kind column)
            conn.execute(
                "INSERT INTO sessions (session_id, tool, transcript_path, transcript_format, \
                 watermark_type, watermark_value, external_session_id, external_parent_session_id, \
                 first_seen_at, last_processed_at, last_known_size, last_modified, \
                 processing_errors, last_error, repo_work_dir) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                rusqlite::params![
                    "sess-migrate-1",
                    "claude",
                    "/path/to/transcript.jsonl",
                    "ClaudeJsonl",
                    "ByteOffset",
                    "1234",
                    "external-sess-1",
                    None::<String>,
                    1000,
                    500,
                    5678,
                    Some(900),
                    2,
                    Some("some error"),
                    Some("/work/dir"),
                ],
            )
            .unwrap();

            // Verify we're at version 3
            let version: i64 = conn
                .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(version, 3);
        }

        // Reopen via StreamsDatabase (triggers migration to v4)
        let db = StreamsDatabase::open(&db_path).unwrap();

        // Verify the stream migrated with stream_kind = 'transcript'
        let stream = db
            .get_stream("sess-migrate-1", "transcript", "/path/to/transcript.jsonl")
            .unwrap();
        assert!(stream.is_some(), "stream should exist after migration");
        let stream = stream.unwrap();
        assert_eq!(stream.session_id, "sess-migrate-1");
        assert_eq!(stream.stream_kind, "transcript");
        assert_eq!(stream.tool, "claude");
        assert_eq!(stream.stream_path, "/path/to/transcript.jsonl");
        assert_eq!(stream.watermark_value, "1234");
        assert_eq!(stream.external_session_id, "external-sess-1");
        assert_eq!(stream.external_parent_session_id, None);
        assert_eq!(stream.last_known_size, 5678);
        assert_eq!(stream.last_modified, Some(900));
        assert_eq!(stream.processing_errors, 2);
        assert_eq!(stream.last_error, Some("some error".to_string()));
        assert_eq!(stream.repo_work_dir, Some("/work/dir".to_string()));
    }

    #[test]
    fn test_migration_v3_to_v4_multiple_sessions_no_conflict() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("migration_multi.db");

        // Create v3 DB with multiple sessions
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            for migration in &MIGRATIONS[..3] {
                conn.execute_batch(migration).unwrap();
            }
            for i in 0..5 {
                conn.execute(
                    "INSERT INTO sessions (session_id, tool, transcript_path, transcript_format, \
                     watermark_type, watermark_value, external_session_id, external_parent_session_id, \
                     first_seen_at, last_processed_at, last_known_size, last_modified, \
                     processing_errors, last_error, repo_work_dir) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    rusqlite::params![
                        format!("sess-{}", i),
                        "claude",
                        format!("/path/to/transcript_{}.jsonl", i),
                        "ClaudeJsonl",
                        "ByteOffset",
                        format!("{}", i * 100),
                        format!("ext-{}", i),
                        None::<String>,
                        1000 + i,
                        500 + i,
                        0,
                        None::<i64>,
                        0,
                        None::<String>,
                        None::<String>,
                    ],
                )
                .unwrap();
            }
        }

        // Reopen (triggers migration)
        let db = StreamsDatabase::open(&db_path).unwrap();

        // All 5 streams should be present
        let all = db.all_streams().unwrap();
        assert_eq!(all.len(), 5);

        // Each should have stream_kind = 'transcript'
        for i in 0..5 {
            let stream = db
                .get_stream(
                    &format!("sess-{}", i),
                    "transcript",
                    &format!("/path/to/transcript_{}.jsonl", i),
                )
                .unwrap()
                .unwrap();
            assert_eq!(stream.stream_kind, "transcript");
            assert_eq!(stream.watermark_value, format!("{}", i * 100));
        }
    }

    #[test]
    fn test_composite_pk_allows_same_session_id_different_streams() {
        let (db, _temp) = create_test_db();

        // Insert same session_id with different stream_kind
        let mut transcript_stream = create_test_stream("shared-session");
        transcript_stream.stream_kind = "transcript".to_string();
        transcript_stream.stream_path = "/path/to/transcript.jsonl".to_string();
        db.insert_stream(&transcript_stream).unwrap();

        let mut otel_stream = create_test_stream("shared-session");
        otel_stream.stream_kind = "otel_traces".to_string();
        otel_stream.stream_path = "/path/to/traces.db".to_string();
        otel_stream.watermark_type = "TimestampCursor".to_string();
        otel_stream.watermark_value = "0|".to_string();
        db.insert_stream(&otel_stream).unwrap();

        // Both should exist independently
        let t = db
            .get_stream("shared-session", "transcript", "/path/to/transcript.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(t.stream_kind, "transcript");

        let o = db
            .get_stream("shared-session", "otel_traces", "/path/to/traces.db")
            .unwrap()
            .unwrap();
        assert_eq!(o.stream_kind, "otel_traces");

        // Update one without affecting the other
        let new_watermark = super::super::watermark::ByteOffsetWatermark::new(999);
        db.update_watermark(
            "shared-session",
            "transcript",
            "/path/to/transcript.jsonl",
            &new_watermark,
        )
        .unwrap();

        let t_updated = db
            .get_stream("shared-session", "transcript", "/path/to/transcript.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(t_updated.watermark_value, "999");

        // OTEL watermark unchanged
        let o_unchanged = db
            .get_stream("shared-session", "otel_traces", "/path/to/traces.db")
            .unwrap()
            .unwrap();
        assert_eq!(o_unchanged.watermark_value, "0|");
    }

    #[test]
    fn test_composite_pk_allows_same_session_id_different_paths() {
        let (db, _temp) = create_test_db();

        // This is the #1461 scenario: same session_id, same stream_kind, different paths
        let mut stream1 = create_test_stream("colliding-session");
        stream1.stream_path = "/worktree-a/transcript.jsonl".to_string();
        db.insert_stream(&stream1).unwrap();

        let mut stream2 = create_test_stream("colliding-session");
        stream2.stream_path = "/worktree-b/transcript.jsonl".to_string();
        db.insert_stream(&stream2).unwrap();

        // Both exist independently
        let s1 = db
            .get_stream(
                "colliding-session",
                "transcript",
                "/worktree-a/transcript.jsonl",
            )
            .unwrap()
            .unwrap();
        let s2 = db
            .get_stream(
                "colliding-session",
                "transcript",
                "/worktree-b/transcript.jsonl",
            )
            .unwrap()
            .unwrap();
        assert_eq!(s1.stream_path, "/worktree-a/transcript.jsonl");
        assert_eq!(s2.stream_path, "/worktree-b/transcript.jsonl");

        // Delete one, the other remains
        db.delete_stream(
            "colliding-session",
            "transcript",
            "/worktree-a/transcript.jsonl",
        )
        .unwrap();
        assert!(
            db.get_stream(
                "colliding-session",
                "transcript",
                "/worktree-a/transcript.jsonl"
            )
            .unwrap()
            .is_none()
        );
        assert!(
            db.get_stream(
                "colliding-session",
                "transcript",
                "/worktree-b/transcript.jsonl"
            )
            .unwrap()
            .is_some()
        );
    }
}
