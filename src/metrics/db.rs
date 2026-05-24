//! Simple metrics storage for offline buffering.
//!
//! Events are stored here when API conditions aren't met.
//! Server handles idempotency - no retry/queue logic needed.

use crate::error::GitAiError;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Current schema version (must match MIGRATIONS.len())
const SCHEMA_VERSION: usize = 4;

/// Database migrations - each migration upgrades the schema by one version
const MIGRATIONS: &[&str] = &[
    // Migration 0 -> 1: Initial schema with metrics table
    r#"
    CREATE TABLE IF NOT EXISTS metrics (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        event_json TEXT NOT NULL
    );
    "#,
    // Migration 1 -> 2: Persistent rate limiter state for agent_usage events
    r#"
    CREATE TABLE IF NOT EXISTS agent_usage_throttle (
        prompt_id TEXT PRIMARY KEY,
        last_sent_ts INTEGER NOT NULL
    );
    "#,
    // Migration 2 -> 3: Persistent local event history for `git-ai activity`
    r#"
    CREATE TABLE IF NOT EXISTS local_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id INTEGER NOT NULL,
        ts INTEGER NOT NULL,
        event_json TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS local_events_ts ON local_events (ts);
    CREATE INDEX IF NOT EXISTS local_events_event_id ON local_events (event_id);
    "#,
    // Migration 3 -> 4: Add repo_url column to local_events for per-repo filtering
    r#"
    ALTER TABLE local_events ADD COLUMN repo_url TEXT;
    CREATE INDEX IF NOT EXISTS local_events_repo_url ON local_events (repo_url);
    "#,
];

/// Global database singleton
static METRICS_DB: OnceLock<Mutex<MetricsDatabase>> = OnceLock::new();

/// Record returned from database queries
#[derive(Debug, Clone)]
pub struct MetricRecord {
    pub id: i64,
    pub event_json: String,
}

/// Record from the local_events table
#[derive(Debug, Clone)]
pub struct LocalEventRecord {
    pub event_id: u16,
    pub ts: u32,
    pub repo_url: Option<String>,
    pub event_json: String,
}

/// Database wrapper for metrics storage
pub struct MetricsDatabase {
    conn: Connection,
}

impl MetricsDatabase {
    /// Get or initialize the global database
    pub fn global() -> Result<&'static Mutex<MetricsDatabase>, GitAiError> {
        let db_mutex = METRICS_DB.get_or_init(|| {
            match Self::new() {
                Ok(db) => Mutex::new(db),
                Err(e) => {
                    eprintln!("[Error] Failed to initialize metrics database: {}", e);
                    // Create a dummy connection that will fail on any operation
                    let temp_path = std::env::temp_dir().join("git-ai-metrics-db-failed");
                    let conn = Connection::open(&temp_path).expect("Failed to create temp DB");
                    Mutex::new(MetricsDatabase { conn })
                }
            }
        });

        Ok(db_mutex)
    }

    /// Create a new database connection
    fn new() -> Result<Self, GitAiError> {
        let db_path = Self::database_path()?;

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Open with WAL mode and performance optimizations
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA cache_size=-2000;
            PRAGMA temp_store=MEMORY;
            "#,
        )?;

        let mut db = Self { conn };
        db.initialize_schema()?;

        Ok(db)
    }

    /// Get database path: ~/.git-ai/internal/metrics-db
    fn database_path() -> Result<PathBuf, GitAiError> {
        // Allow test override via environment variable
        #[cfg(any(test, feature = "test-support"))]
        if let Ok(test_path) = std::env::var("GIT_AI_TEST_METRICS_DB_PATH") {
            return Ok(PathBuf::from(test_path));
        }

        let home = dirs::home_dir()
            .ok_or_else(|| GitAiError::Generic("Could not determine home directory".to_string()))?;
        Ok(home.join(".git-ai").join("internal").join("metrics-db"))
    }

    /// Initialize schema and handle migrations
    fn initialize_schema(&mut self) -> Result<(), GitAiError> {
        // FAST PATH: Check if database is already at current version
        let version_check: Result<usize, _> = self.conn.query_row(
            "SELECT value FROM schema_metadata WHERE key = 'version'",
            [],
            |row| {
                let version_str: String = row.get(0)?;
                version_str
                    .parse::<usize>()
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            },
        );

        if let Ok(current_version) = version_check {
            if current_version == SCHEMA_VERSION {
                return Ok(());
            }
            if current_version > SCHEMA_VERSION {
                return Err(GitAiError::Generic(format!(
                    "Metrics database schema version {} is newer than supported version {}. \
                     Please upgrade git-ai to the latest version.",
                    current_version, SCHEMA_VERSION
                )));
            }
        }

        // Create schema_metadata table
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_metadata (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            "#,
        )?;

        // Get current schema version (0 if brand new database)
        let current_version: usize = self
            .conn
            .query_row(
                "SELECT value FROM schema_metadata WHERE key = 'version'",
                [],
                |row| {
                    let version_str: String = row.get(0)?;
                    version_str
                        .parse::<usize>()
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
                },
            )
            .unwrap_or(0);

        // Apply all missing migrations sequentially
        for target_version in current_version..SCHEMA_VERSION {
            self.apply_migration(target_version)?;

            // Use an upsert so concurrent initializers do not race on version row creation.
            self.conn.execute(
                r#"
                INSERT INTO schema_metadata (key, value)
                VALUES ('version', ?1)
                ON CONFLICT(key) DO UPDATE SET
                    value = excluded.value
                WHERE CAST(schema_metadata.value AS INTEGER) < CAST(excluded.value AS INTEGER)
                "#,
                params![(target_version + 1).to_string()],
            )?;
        }

        Ok(())
    }

    /// Apply a single migration
    fn apply_migration(&mut self, from_version: usize) -> Result<(), GitAiError> {
        if from_version >= MIGRATIONS.len() {
            return Err(GitAiError::Generic(format!(
                "No migration defined for version {} -> {}",
                from_version,
                from_version + 1
            )));
        }

        let migration_sql = MIGRATIONS[from_version];
        let tx = self.conn.transaction()?;
        tx.execute_batch(migration_sql)?;
        tx.commit()?;

        Ok(())
    }

    /// Insert events as JSON strings
    pub fn insert_events(&mut self, events: &[String]) -> Result<(), GitAiError> {
        if events.is_empty() {
            return Ok(());
        }

        let tx = self.conn.transaction()?;

        {
            let mut stmt = tx.prepare_cached("INSERT INTO metrics (event_json) VALUES (?1)")?;

            for event_json in events {
                stmt.execute(params![event_json])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Get batch of events (oldest first)
    pub fn get_batch(&self, limit: usize) -> Result<Vec<MetricRecord>, GitAiError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, event_json FROM metrics ORDER BY id ASC LIMIT ?1")?;

        let rows = stmt.query_map(params![limit], |row| {
            Ok(MetricRecord {
                id: row.get(0)?,
                event_json: row.get(1)?,
            })
        })?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }

        Ok(records)
    }

    /// Delete records by ID (after successful upload)
    pub fn delete_records(&mut self, ids: &[i64]) -> Result<(), GitAiError> {
        if ids.is_empty() {
            return Ok(());
        }

        let tx = self.conn.transaction()?;

        {
            let mut stmt = tx.prepare_cached("DELETE FROM metrics WHERE id = ?1")?;

            for id in ids {
                stmt.execute(params![id])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Get count of pending metrics
    pub fn count(&self) -> Result<usize, GitAiError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// How long local_events rows are retained (30 days in seconds).
    const LOCAL_EVENTS_RETENTION_SECS: u64 = 30 * 24 * 3600;
    /// Minimum interval between prune passes (24 hours in seconds).
    const LOCAL_EVENTS_PRUNE_INTERVAL_SECS: u64 = 24 * 3600;

    /// Insert events into the local_events table.
    ///
    /// Each tuple is (event_id, ts, repo_url, event_json). Call this with events
    /// filtered to only the interesting event types before inserting.
    /// Opportunistically prunes rows older than 30 days at most once per day.
    pub fn insert_local_events(
        &mut self,
        events: &[(u16, u32, Option<String>, String)],
    ) -> Result<(), GitAiError> {
        if events.is_empty() {
            return Ok(());
        }

        let tx = self.conn.transaction()?;

        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO local_events (event_id, ts, repo_url, event_json) VALUES (?1, ?2, ?3, ?4)",
            )?;

            for (event_id, ts, repo_url, json) in events {
                stmt.execute(params![
                    *event_id as i64,
                    *ts as i64,
                    repo_url.as_deref(),
                    json
                ])?;
            }
        }

        tx.commit()?;
        self.prune_local_events_if_due()?;
        Ok(())
    }

    /// Delete local_events rows older than `LOCAL_EVENTS_RETENTION_SECS`, but
    /// only if the last prune was more than `LOCAL_EVENTS_PRUNE_INTERVAL_SECS` ago.
    fn prune_local_events_if_due(&mut self) -> Result<(), GitAiError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let last_prune: Option<i64> = self
            .conn
            .query_row(
                "SELECT value FROM schema_metadata WHERE key = 'local_events_last_prune_ts'",
                [],
                |row| row.get(0),
            )
            .optional()?
            .and_then(|v: String| v.parse().ok());

        if let Some(last) = last_prune {
            if now.saturating_sub(last as u64) < Self::LOCAL_EVENTS_PRUNE_INTERVAL_SECS {
                return Ok(());
            }
        }

        let cutoff = now.saturating_sub(Self::LOCAL_EVENTS_RETENTION_SECS);
        self.conn.execute(
            "DELETE FROM local_events WHERE ts < ?1",
            params![cutoff as i64],
        )?;
        self.conn.execute(
            "INSERT OR REPLACE INTO schema_metadata (key, value) VALUES ('local_events_last_prune_ts', ?1)",
            params![now.to_string()],
        )?;

        Ok(())
    }

    /// Query local_events since `since_ts` (Unix seconds), returning all interesting event types.
    ///
    /// When `repo_filter` is `Some(url)`, only events matching that repo_url are returned.
    /// An empty string `""` is a sentinel meaning "events with no repo_url (NULL)".
    /// When `None`, all events are returned regardless of repo.
    pub fn get_local_events(
        &self,
        since_ts: u32,
        repo_filter: Option<&str>,
    ) -> Result<Vec<LocalEventRecord>, GitAiError> {
        // Shared row mapper used by all three query branches below.
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(LocalEventRecord {
                event_id: row.get::<_, i64>(0)? as u16,
                ts: row.get::<_, i64>(1)? as u32,
                repo_url: row.get(2)?,
                event_json: row.get(3)?,
            })
        };

        let records = if let Some(repo_url) = repo_filter {
            if repo_url.is_empty() {
                let mut stmt = self.conn.prepare(
                    "SELECT event_id, ts, repo_url, event_json FROM local_events \
                     WHERE ts >= ?1 AND repo_url IS NULL \
                     ORDER BY ts ASC",
                )?;
                stmt.query_map(params![since_ts as i64], map_row)?
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                // Escape LIKE special characters so a user-supplied substring
                // like "my_org/my%repo" matches literally, not as wildcards.
                // We use '\' as the escape character.
                let escaped = repo_url.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
                let pattern = format!("%{}%", escaped);
                let mut stmt = self.conn.prepare(
                    "SELECT event_id, ts, repo_url, event_json FROM local_events \
                     WHERE ts >= ?1 AND repo_url LIKE ?2 ESCAPE '\\' \
                     ORDER BY ts ASC",
                )?;
                stmt.query_map(params![since_ts as i64, pattern], map_row)?
                    .collect::<Result<Vec<_>, _>>()?
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT event_id, ts, repo_url, event_json FROM local_events \
                 WHERE ts >= ?1 \
                 ORDER BY ts ASC",
            )?;
            stmt.query_map(params![since_ts as i64], map_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(records)
    }

    /// Returns whether an `agent_usage` event should be emitted for this prompt_id.
    ///
    /// If emitted, this method also updates the prompt's last-sent timestamp.
    pub fn should_emit_agent_usage(
        &mut self,
        prompt_id: &str,
        now_ts: u64,
        min_interval_secs: u64,
    ) -> Result<bool, GitAiError> {
        if prompt_id.is_empty() {
            return Ok(true);
        }

        let tx = self.conn.transaction()?;
        let existing_ts: Option<i64> = tx
            .query_row(
                "SELECT last_sent_ts FROM agent_usage_throttle WHERE prompt_id = ?1",
                params![prompt_id],
                |row| row.get(0),
            )
            .optional()?;

        let should_emit = existing_ts
            .map(|prev_ts| now_ts.saturating_sub(prev_ts as u64) >= min_interval_secs)
            .unwrap_or(true);

        if should_emit {
            tx.execute(
                r#"
                INSERT INTO agent_usage_throttle (prompt_id, last_sent_ts)
                VALUES (?1, ?2)
                ON CONFLICT(prompt_id) DO UPDATE SET last_sent_ts = excluded.last_sent_ts
                "#,
                params![prompt_id, now_ts as i64],
            )?;
        }

        tx.commit()?;
        Ok(should_emit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_db() -> (MetricsDatabase, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test-metrics.db");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();

        let mut db = MetricsDatabase { conn };
        db.initialize_schema().unwrap();

        (db, temp_dir)
    }

    #[test]
    fn test_initialize_schema() {
        let (db, _temp_dir) = create_test_db();

        // Verify metrics table exists
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='metrics'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Verify schema_metadata exists with correct version
        let version: String = db
            .conn
            .query_row(
                "SELECT value FROM schema_metadata WHERE key = 'version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "4");
    }

    #[test]
    fn test_initialize_schema_handles_preexisting_agent_usage_table() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("concurrent-init.db");
        let conn = Connection::open(&db_path).unwrap();

        // Simulate a partial migration state from a concurrent process:
        // schema version indicates agent_usage_throttle is missing, but it already exists.
        conn.execute_batch(
            r#"
            CREATE TABLE schema_metadata (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            INSERT INTO schema_metadata (key, value) VALUES ('version', '1');
            CREATE TABLE agent_usage_throttle (
                tool TEXT PRIMARY KEY NOT NULL,
                agent_last_seen_at INTEGER NOT NULL,
                command_last_seen_at INTEGER NOT NULL
            );
            "#,
        )
        .unwrap();

        let mut db = MetricsDatabase { conn };
        db.initialize_schema().unwrap();

        let version: String = db
            .conn
            .query_row(
                "SELECT value FROM schema_metadata WHERE key = 'version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "4");
    }

    #[test]
    fn test_insert_events() {
        let (mut db, _temp_dir) = create_test_db();

        let events = vec![
            r#"{"t":1234567890,"e":1,"v":{"0":"abc123"},"a":{"0":"1.0.0"}}"#.to_string(),
            r#"{"t":1234567891,"e":1,"v":{"0":"def456"},"a":{"0":"1.0.0"}}"#.to_string(),
        ];

        db.insert_events(&events).unwrap();

        let count = db.count().unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_get_batch() {
        let (mut db, _temp_dir) = create_test_db();

        let events = vec![
            r#"{"t":1,"e":1,"v":{},"a":{}}"#.to_string(),
            r#"{"t":2,"e":1,"v":{},"a":{}}"#.to_string(),
            r#"{"t":3,"e":1,"v":{},"a":{}}"#.to_string(),
        ];

        db.insert_events(&events).unwrap();

        // Get batch of 2
        let batch = db.get_batch(2).unwrap();
        assert_eq!(batch.len(), 2);

        // Verify order (oldest first)
        assert!(batch[0].id < batch[1].id);
        assert!(batch[0].event_json.contains("\"t\":1"));
        assert!(batch[1].event_json.contains("\"t\":2"));
    }

    #[test]
    fn test_delete_records() {
        let (mut db, _temp_dir) = create_test_db();

        let events = vec![
            r#"{"t":1,"e":1,"v":{},"a":{}}"#.to_string(),
            r#"{"t":2,"e":1,"v":{},"a":{}}"#.to_string(),
            r#"{"t":3,"e":1,"v":{},"a":{}}"#.to_string(),
        ];

        db.insert_events(&events).unwrap();

        // Get batch and delete first two
        let batch = db.get_batch(2).unwrap();
        let ids: Vec<i64> = batch.iter().map(|r| r.id).collect();

        db.delete_records(&ids).unwrap();

        // Verify only one remains
        let count = db.count().unwrap();
        assert_eq!(count, 1);

        // Verify remaining is the third one
        let remaining = db.get_batch(10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0].event_json.contains("\"t\":3"));
    }

    #[test]
    fn test_empty_operations() {
        let (mut db, _temp_dir) = create_test_db();

        // Insert empty should succeed
        db.insert_events(&[]).unwrap();

        // Get from empty should return empty
        let batch = db.get_batch(10).unwrap();
        assert!(batch.is_empty());

        // Delete empty should succeed
        db.delete_records(&[]).unwrap();

        // Count empty should return 0
        let count = db.count().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_database_path() {
        let path = MetricsDatabase::database_path().unwrap();
        assert!(path.to_string_lossy().contains(".git-ai"));
        assert!(path.to_string_lossy().contains("internal"));
        assert!(path.to_string_lossy().ends_with("metrics-db"));
    }

    #[test]
    fn test_should_emit_agent_usage_rate_limit() {
        let (mut db, _temp_dir) = create_test_db();
        let prompt_id = "prompt-123";

        // First event for a prompt should be allowed.
        assert!(
            db.should_emit_agent_usage(prompt_id, 1_700_000_000, 300)
                .unwrap()
        );
        // Subsequent event inside the window should be throttled.
        assert!(
            !db.should_emit_agent_usage(prompt_id, 1_700_000_120, 300)
                .unwrap()
        );
        // Event outside the window should be allowed again.
        assert!(
            db.should_emit_agent_usage(prompt_id, 1_700_000_301, 300)
                .unwrap()
        );
    }
}
