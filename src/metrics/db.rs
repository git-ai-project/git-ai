//! Metrics storage for local history and offline buffering.
//!
//! Every metric event is stored here. `delivered_ts IS NULL` means the row is
//! still pending upload; delivered rows are retained as the local history.
//! Server handles idempotency.

use crate::error::GitAiError;
use crate::metrics::attrs::attr_pos;
use crate::metrics::pos_encoded::sparse_get_string;
use crate::metrics::types::MetricEvent;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Current schema version (must match MIGRATIONS.len())
const SCHEMA_VERSION: usize = 6;

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
    // Migration 2 -> 3: Reserved for a removed local_events design.
    r#"
    "#,
    // Migration 3 -> 4: Reserved for a removed local_events repo_url migration.
    r#"
    "#,
    // Migration 4 -> 5: Keep delivered metrics in the authoritative metrics table.
    r#"
    CREATE TABLE IF NOT EXISTS metrics (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        event_json TEXT NOT NULL
    );
    ALTER TABLE metrics ADD COLUMN delivered_ts INTEGER;
    "#,
    // Migration 5 -> 6: Speed pending queue scans.
    r#"
    CREATE INDEX IF NOT EXISTS metrics_delivered_ts_id ON metrics (delivered_ts, id);
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

/// Record returned for local usage aggregation from the metrics table.
#[derive(Debug, Clone)]
pub struct MetricHistoryRecord {
    pub event_id: u16,
    pub ts: u32,
    pub repo_url: Option<String>,
    pub event: MetricEvent,
}

/// Database wrapper for metrics storage
pub struct MetricsDatabase {
    conn: Connection,
}

impl MetricsDatabase {
    /// How long metric rows are retained for local history/offline retry (45 days).
    const METRICS_RETENTION_SECS: u64 = 45 * 24 * 3600;
    /// Minimum interval between prune passes (24 hours).
    const METRICS_PRUNE_INTERVAL_SECS: u64 = 24 * 3600;

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

    #[cfg(test)]
    pub(crate) fn new_in_memory_for_tests() -> Result<Self, GitAiError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
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
        match tx.execute_batch(migration_sql) {
            Ok(()) => {}
            Err(e) if e.to_string().contains("duplicate column name") => {
                // Another process already applied this ALTER TABLE concurrently.
            }
            Err(e) => return Err(e.into()),
        }
        tx.commit()?;

        Ok(())
    }

    /// Insert undelivered events as JSON strings.
    pub fn insert_events(&mut self, events: &[String]) -> Result<Vec<i64>, GitAiError> {
        self.insert_events_with_delivered_ts(events, None)
    }

    /// Insert events as JSON strings, optionally marking them delivered immediately.
    pub fn insert_events_with_delivered_ts(
        &mut self,
        events: &[String],
        delivered_ts: Option<u64>,
    ) -> Result<Vec<i64>, GitAiError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }

        let tx = self.conn.transaction()?;
        let mut ids = Vec::with_capacity(events.len());

        {
            let mut stmt = tx.prepare_cached("INSERT INTO metrics (event_json) VALUES (?1)")?;
            let mut delivered_stmt = tx
                .prepare_cached("INSERT INTO metrics (event_json, delivered_ts) VALUES (?1, ?2)")?;

            for event_json in events {
                if let Some(ts) = delivered_ts {
                    delivered_stmt.execute(params![event_json, ts as i64])?;
                } else {
                    stmt.execute(params![event_json])?;
                }
                ids.push(tx.last_insert_rowid());
            }
        }

        tx.commit()?;
        self.prune_old_metrics_if_due()?;
        Ok(ids)
    }

    /// Get a batch of undelivered events (oldest first).
    pub fn get_batch(&self, limit: usize) -> Result<Vec<MetricRecord>, GitAiError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, event_json FROM metrics \
             WHERE delivered_ts IS NULL \
             ORDER BY id ASC LIMIT ?1",
        )?;

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

    /// Mark records as delivered after a successful upload.
    pub fn mark_records_delivered(
        &mut self,
        ids: &[i64],
        delivered_ts: u64,
    ) -> Result<(), GitAiError> {
        if ids.is_empty() {
            return Ok(());
        }

        let tx = self.conn.transaction()?;

        {
            let mut stmt = tx.prepare_cached(
                "UPDATE metrics SET delivered_ts = ?1 WHERE id = ?2 AND delivered_ts IS NULL",
            )?;

            for id in ids {
                stmt.execute(params![delivered_ts as i64, id])?;
            }
        }

        tx.commit()?;
        self.prune_old_metrics_if_due()?;
        Ok(())
    }

    /// Delete metric rows outside the local retention window.
    ///
    /// Valid rows are pruned by event timestamp, regardless of delivery state. Malformed
    /// rows cannot be aged by event timestamp, so delivered malformed rows fall back to
    /// `delivered_ts`.
    fn prune_old_metrics_if_due(&mut self) -> Result<(), GitAiError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let last_prune: Option<i64> = self
            .conn
            .query_row(
                "SELECT value FROM schema_metadata WHERE key = 'metrics_last_prune_ts'",
                [],
                |row| row.get(0),
            )
            .optional()?
            .and_then(|v: String| v.parse().ok());

        if let Some(last) = last_prune
            && now.saturating_sub(last as u64) < Self::METRICS_PRUNE_INTERVAL_SECS
        {
            return Ok(());
        }

        let cutoff = now.saturating_sub(Self::METRICS_RETENTION_SECS);
        let rows_to_prune = self.old_metric_row_ids(cutoff)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO schema_metadata (key, value) VALUES ('metrics_last_prune_ts', ?1)",
            params![now.to_string()],
        )?;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM metrics WHERE id = ?1")?;
            for id in rows_to_prune {
                stmt.execute(params![id])?;
            }
        }
        tx.commit()?;

        Ok(())
    }

    fn old_metric_row_ids(&self, cutoff: u64) -> Result<Vec<i64>, GitAiError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, event_json, delivered_ts FROM metrics ORDER BY id ASC")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?;

        let mut ids = Vec::new();
        for row in rows {
            let (id, event_json, delivered_ts) = row?;
            if metric_row_is_older_than_cutoff(&event_json, delivered_ts, cutoff) {
                ids.push(id);
            }
        }

        Ok(ids)
    }

    /// Get count of pending metrics.
    pub fn count(&self) -> Result<usize, GitAiError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM metrics WHERE delivered_ts IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Query persisted metric rows since `since_ts` (Unix seconds).
    ///
    /// When `repo_filter` is `Some(url)`, only events matching that repo_url are returned.
    /// An empty string `""` is a sentinel meaning "events with no repo_url (NULL)".
    /// When `None`, all events are returned regardless of repo.
    pub fn get_metric_history(
        &self,
        since_ts: u32,
        repo_filter: Option<&str>,
        event_ids: &[u16],
    ) -> Result<Vec<MetricHistoryRecord>, GitAiError> {
        let mut stmt = self
            .conn
            .prepare("SELECT event_json FROM metrics ORDER BY id ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

        let mut records = Vec::new();
        for row in rows {
            let event_json = row?;
            let Ok(event) = serde_json::from_str::<MetricEvent>(&event_json) else {
                continue;
            };

            if event.timestamp < since_ts || !event_ids.contains(&event.event_id) {
                continue;
            }

            let repo_url = sparse_get_string(&event.attrs, attr_pos::REPO_URL).flatten();
            let repo_matches = match repo_filter {
                None => true,
                Some("") => repo_url.is_none(),
                Some(filter) => repo_url.as_deref().is_some_and(|url| url.contains(filter)),
            };
            if !repo_matches {
                continue;
            }

            records.push(MetricHistoryRecord {
                event_id: event.event_id,
                ts: event.timestamp,
                repo_url,
                event,
            });
        }

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

fn metric_row_is_older_than_cutoff(
    event_json: &str,
    delivered_ts: Option<i64>,
    cutoff: u64,
) -> bool {
    if let Ok(event) = serde_json::from_str::<MetricTimestampOnly>(event_json) {
        return u64::from(event.timestamp) < cutoff;
    }

    delivered_ts.is_some_and(|ts| ts >= 0 && (ts as u64) < cutoff)
}

#[derive(Deserialize)]
struct MetricTimestampOnly {
    #[serde(rename = "t")]
    timestamp: u32,
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

    fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn days_ago(days: u64) -> u32 {
        unix_now()
            .saturating_sub(days * 24 * 3600)
            .min(u32::MAX as u64) as u32
    }

    fn event_json(ts: u32) -> String {
        format!(r#"{{"t":{ts},"e":1,"v":{{}},"a":{{}}}}"#)
    }

    fn event_json_with_repo(ts: u32, event_id: u16, repo: &str) -> String {
        format!(r#"{{"t":{ts},"e":{event_id},"v":{{}},"a":{{"1":"{repo}"}}}}"#)
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
        assert_eq!(version, "6");

        // Verify delivered_ts exists on the authoritative metrics table.
        let delivered_ts_columns: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('metrics') WHERE name = 'delivered_ts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(delivered_ts_columns, 1);
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
        assert_eq!(version, "6");
    }

    #[test]
    fn test_insert_events() {
        let (mut db, _temp_dir) = create_test_db();
        let ts1 = days_ago(2);
        let ts2 = days_ago(1);

        let events = vec![
            format!(r#"{{"t":{ts1},"e":1,"v":{{"0":"abc123"}},"a":{{"0":"1.0.0"}}}}"#),
            format!(r#"{{"t":{ts2},"e":1,"v":{{"0":"def456"}},"a":{{"0":"1.0.0"}}}}"#),
        ];

        let ids = db.insert_events(&events).unwrap();

        let count = db.count().unwrap();
        assert_eq!(count, 2);
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn test_get_batch() {
        let (mut db, _temp_dir) = create_test_db();
        let ts1 = days_ago(3);
        let ts2 = days_ago(2);
        let ts3 = days_ago(1);

        let events = vec![event_json(ts1), event_json(ts2), event_json(ts3)];

        db.insert_events(&events).unwrap();

        // Get batch of 2
        let batch = db.get_batch(2).unwrap();
        assert_eq!(batch.len(), 2);

        // Verify order (oldest first)
        assert!(batch[0].id < batch[1].id);
        assert!(batch[0].event_json.contains(&format!("\"t\":{ts1}")));
        assert!(batch[1].event_json.contains(&format!("\"t\":{ts2}")));
    }

    #[test]
    fn test_mark_records_delivered() {
        let (mut db, _temp_dir) = create_test_db();
        let ts1 = days_ago(3);
        let ts2 = days_ago(2);
        let ts3 = days_ago(1);

        let events = vec![event_json(ts1), event_json(ts2), event_json(ts3)];

        db.insert_events(&events).unwrap();

        // Get batch and mark first two delivered.
        let batch = db.get_batch(2).unwrap();
        let ids: Vec<i64> = batch.iter().map(|r| r.id).collect();

        db.mark_records_delivered(&ids, unix_now()).unwrap();

        // Verify only one remains pending.
        let count = db.count().unwrap();
        assert_eq!(count, 1);

        // Verify remaining pending row is the third one.
        let remaining = db.get_batch(10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0].event_json.contains(&format!("\"t\":{ts3}")));

        // Verify delivered rows are retained.
        let total: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 3);
    }

    #[test]
    fn test_insert_events_with_delivered_ts_skips_batch() {
        let (mut db, _temp_dir) = create_test_db();

        let delivered_ts = unix_now();
        let delivered_event_ts = days_ago(2);
        let pending_event_ts = days_ago(1);
        let delivered = vec![event_json(delivered_event_ts)];
        let pending = vec![event_json(pending_event_ts)];

        db.insert_events_with_delivered_ts(&delivered, Some(delivered_ts))
            .unwrap();
        db.insert_events(&pending).unwrap();

        let batch = db.get_batch(10).unwrap();
        assert_eq!(batch.len(), 1);
        assert!(
            batch[0]
                .event_json
                .contains(&format!("\"t\":{pending_event_ts}"))
        );
        assert_eq!(db.count().unwrap(), 1);

        let total: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 2);
    }

    #[test]
    fn test_get_metric_history_reads_authoritative_metrics_table() {
        let (mut db, _temp_dir) = create_test_db();

        let delivered_ts = unix_now();
        let ts1 = days_ago(4);
        let ts2 = days_ago(3);
        let ts3 = days_ago(2);
        let ts4 = days_ago(1);
        let delivered = vec![event_json_with_repo(
            ts1,
            1,
            "https://github.com/acme/project",
        )];
        let pending = vec![
            event_json_with_repo(ts2, 4, "https://github.com/acme/project"),
            event_json_with_repo(ts3, 2, "https://github.com/acme/project"),
            event_json_with_repo(ts4, 5, "https://github.com/other/repo"),
        ];

        db.insert_events_with_delivered_ts(&delivered, Some(delivered_ts))
            .unwrap();
        db.insert_events(&pending).unwrap();

        let records = db
            .get_metric_history(0, Some("acme/project"), &[1, 4, 5])
            .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].event_id, 1);
        assert_eq!(records[0].ts, ts1);
        assert_eq!(records[1].event_id, 4);
        assert_eq!(records[1].ts, ts2);

        // Delivered rows are retained for history, but only undelivered rows flush.
        let batch = db.get_batch(10).unwrap();
        assert_eq!(batch.len(), 3);
    }

    #[test]
    fn test_prunes_metric_rows_older_than_retention_by_event_timestamp() {
        let (mut db, _temp_dir) = create_test_db();

        let delivered_ts = unix_now();
        let old_event_ts = days_ago(46);
        let recent_event_ts = days_ago(44);
        let events = vec![event_json(old_event_ts), event_json(recent_event_ts)];

        db.insert_events_with_delivered_ts(&events, Some(delivered_ts))
            .unwrap();

        let total_after_prune: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total_after_prune, 1);

        let records = db.get_metric_history(0, None, &[1]).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].ts, recent_event_ts);
    }

    #[test]
    fn test_prunes_old_pending_metric_rows() {
        let (mut db, _temp_dir) = create_test_db();

        let old_event_ts = days_ago(46);
        let recent_event_ts = days_ago(1);
        let pending = vec![event_json(old_event_ts), event_json(recent_event_ts)];

        db.insert_events(&pending).unwrap();

        let total: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(db.count().unwrap(), 1);

        let batch = db.get_batch(10).unwrap();
        assert_eq!(batch.len(), 1);
        assert!(
            batch[0]
                .event_json
                .contains(&format!("\"t\":{recent_event_ts}"))
        );
    }

    #[test]
    fn test_prunes_malformed_delivered_rows_by_delivered_timestamp() {
        let (mut db, _temp_dir) = create_test_db();

        let old_delivered_ts =
            unix_now().saturating_sub(MetricsDatabase::METRICS_RETENTION_SECS + 1);
        db.insert_events_with_delivered_ts(&["not-json".to_string()], Some(old_delivered_ts))
            .unwrap();

        let total: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 0);
    }

    #[test]
    fn test_empty_operations() {
        let (mut db, _temp_dir) = create_test_db();

        // Insert empty should succeed
        db.insert_events(&[]).unwrap();

        // Get from empty should return empty
        let batch = db.get_batch(10).unwrap();
        assert!(batch.is_empty());

        // Marking an empty set delivered should succeed.
        db.mark_records_delivered(&[], 1_700_000_000).unwrap();

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
