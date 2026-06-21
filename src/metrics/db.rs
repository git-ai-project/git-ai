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
const SCHEMA_VERSION: usize = 3;

const MAX_METRIC_UPLOAD_ATTEMPTS: u32 = 6;
const METRIC_PROCESSING_LOCK_TIMEOUT_SECS: u64 = 10 * 60;

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
    // Migration 2 -> 3: Keep delivered metrics and add row-level retry state.
    r#"
    CREATE INDEX IF NOT EXISTS metrics_pending_retry
        ON metrics (delivered_ts, next_retry_at, id)
        WHERE delivered_ts IS NULL;

    CREATE INDEX IF NOT EXISTS metrics_processing_started_at
        ON metrics (processing_started_at)
        WHERE delivered_ts IS NULL AND processing_started_at IS NOT NULL;
    "#,
];

/// Global database singleton
static METRICS_DB: OnceLock<Mutex<MetricsDatabase>> = OnceLock::new();

/// Record returned from database queries
#[derive(Debug, Clone)]
pub struct MetricRecord {
    pub id: i64,
    pub event_json: String,
    pub attempts: u32,
    pub next_retry_at: u64,
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
    /// How long metric rows are retained for local history/offline retry (365 days).
    const METRICS_RETENTION_SECS: u64 = 365 * 24 * 3600;
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

        if from_version == 2 {
            self.add_row_level_retry_columns()?;
        }

        let migration_sql = MIGRATIONS[from_version];
        let tx = self.conn.transaction()?;
        tx.execute_batch(migration_sql)?;
        tx.commit()?;

        Ok(())
    }

    fn add_row_level_retry_columns(&mut self) -> Result<(), GitAiError> {
        for (name, sql) in [
            (
                "delivered_ts",
                "ALTER TABLE metrics ADD COLUMN delivered_ts INTEGER",
            ),
            (
                "attempts",
                "ALTER TABLE metrics ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0",
            ),
            (
                "last_sync_error",
                "ALTER TABLE metrics ADD COLUMN last_sync_error TEXT",
            ),
            (
                "last_sync_at",
                "ALTER TABLE metrics ADD COLUMN last_sync_at INTEGER",
            ),
            (
                "next_retry_at",
                "ALTER TABLE metrics ADD COLUMN next_retry_at INTEGER NOT NULL DEFAULT 0",
            ),
            (
                "processing_started_at",
                "ALTER TABLE metrics ADD COLUMN processing_started_at INTEGER",
            ),
        ] {
            self.add_column_if_missing("metrics", name, sql)?;
        }
        Ok(())
    }

    fn add_column_if_missing(
        &mut self,
        table: &str,
        column: &str,
        alter_sql: &str,
    ) -> Result<(), GitAiError> {
        if self.column_exists(table, column)? {
            return Ok(());
        }

        match self.conn.execute(alter_sql, []) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(_, Some(message)))
                if message.contains("duplicate column name") =>
            {
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    fn column_exists(&self, table: &str, column: &str) -> Result<bool, GitAiError> {
        let count: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
            params![column],
            |row| row.get(0),
        )?;
        Ok(count > 0)
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

    /// Atomically claim a due batch of pending metrics for upload.
    pub fn dequeue_pending_batch(&mut self, limit: usize) -> Result<Vec<MetricRecord>, GitAiError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let now = current_unix_ts();
        self.release_stale_processing_locks(now)?;

        let tx = self.conn.transaction()?;
        let ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM metrics \
                 WHERE delivered_ts IS NULL \
                   AND processing_started_at IS NULL \
                   AND next_retry_at <= ?1 \
                   AND attempts < ?2 \
                 ORDER BY next_retry_at ASC, id DESC \
                 LIMIT ?3",
            )?;
            let rows = stmt.query_map(
                params![now as i64, MAX_METRIC_UPLOAD_ATTEMPTS as i64, limit as i64],
                |row| row.get::<_, i64>(0),
            )?;
            let mut ids = Vec::new();
            for row in rows {
                ids.push(row?);
            }
            ids
        };

        if ids.is_empty() {
            tx.commit()?;
            return Ok(Vec::new());
        }

        let mut locked_ids = Vec::with_capacity(ids.len());
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE metrics \
                 SET processing_started_at = ?1 \
                 WHERE id = ?2 \
                   AND delivered_ts IS NULL \
                   AND processing_started_at IS NULL",
            )?;
            for id in ids {
                if stmt.execute(params![now as i64, id])? > 0 {
                    locked_ids.push(id);
                }
            }
        }

        let mut records = Vec::with_capacity(locked_ids.len());
        {
            let mut stmt = tx.prepare_cached(
                "SELECT id, event_json, attempts, next_retry_at FROM metrics WHERE id = ?1",
            )?;
            for id in locked_ids {
                records.push(stmt.query_row(params![id], |row| {
                    Ok(MetricRecord {
                        id: row.get(0)?,
                        event_json: row.get(1)?,
                        attempts: row.get::<_, i64>(2)?.max(0) as u32,
                        next_retry_at: row.get::<_, i64>(3)?.max(0) as u64,
                    })
                })?);
            }
        }

        tx.commit()?;
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
                "UPDATE metrics \
                 SET delivered_ts = ?1, processing_started_at = NULL \
                 WHERE id = ?2 AND delivered_ts IS NULL",
            )?;

            for id in ids {
                stmt.execute(params![delivered_ts as i64, id])?;
            }
        }

        tx.commit()?;
        self.prune_old_metrics_if_due()?;
        Ok(())
    }

    /// Mark records as failed and schedule their next row-level retry.
    pub fn mark_records_failed(
        &mut self,
        ids: &[i64],
        error: &str,
        failed_at: u64,
    ) -> Result<(), GitAiError> {
        if ids.is_empty() {
            return Ok(());
        }

        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                r#"
                UPDATE metrics
                SET processing_started_at = NULL,
                    attempts = attempts + 1,
                    last_sync_error = ?1,
                    last_sync_at = ?2,
                    next_retry_at = ?2 + CASE
                        WHEN attempts + 1 <= 1 THEN 300
                        WHEN attempts + 1 = 2 THEN 1800
                        WHEN attempts + 1 = 3 THEN 7200
                        WHEN attempts + 1 = 4 THEN 21600
                        WHEN attempts + 1 = 5 THEN 43200
                        ELSE 86400
                    END
                WHERE id = ?3 AND delivered_ts IS NULL
                "#,
            )?;

            for id in ids {
                stmt.execute(params![error, failed_at as i64, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Mark records as permanently undeliverable while retaining them in history.
    pub fn mark_records_undeliverable(
        &mut self,
        records: &[(i64, String)],
        failed_at: u64,
    ) -> Result<(), GitAiError> {
        if records.is_empty() {
            return Ok(());
        }

        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE metrics \
                 SET processing_started_at = NULL, \
                     attempts = ?1, \
                     last_sync_error = ?2, \
                     last_sync_at = ?3, \
                     next_retry_at = ?3 \
                 WHERE id = ?4 AND delivered_ts IS NULL",
            )?;

            for (id, error) in records {
                stmt.execute(params![
                    MAX_METRIC_UPLOAD_ATTEMPTS as i64,
                    error,
                    failed_at as i64,
                    id
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Get count of pending metrics that are currently eligible for upload.
    pub fn count_retryable(&self) -> Result<usize, GitAiError> {
        let now = current_unix_ts();
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM metrics \
             WHERE delivered_ts IS NULL \
               AND processing_started_at IS NULL \
               AND next_retry_at <= ?1 \
               AND attempts < ?2",
            params![now as i64, MAX_METRIC_UPLOAD_ATTEMPTS as i64],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    fn release_stale_processing_locks(&mut self, now: u64) -> Result<(), GitAiError> {
        let stale_before = now.saturating_sub(METRIC_PROCESSING_LOCK_TIMEOUT_SECS);
        self.conn.execute(
            "UPDATE metrics \
             SET processing_started_at = NULL \
             WHERE delivered_ts IS NULL \
               AND processing_started_at IS NOT NULL \
               AND processing_started_at < ?1",
            params![stale_before as i64],
        )?;
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

fn current_unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
        seconds_ago(days * 24 * 3600)
    }

    fn seconds_ago(seconds: u64) -> u32 {
        unix_now().saturating_sub(seconds).min(u32::MAX as u64) as u32
    }

    fn event_json(ts: u32) -> String {
        format!(r#"{{"t":{ts},"e":1,"v":{{}},"a":{{}}}}"#)
    }

    fn event_json_with_repo(ts: u32, event_id: u16, repo: &str) -> String {
        format!(r#"{{"t":{ts},"e":{event_id},"v":{{}},"a":{{"1":"{repo}"}}}}"#)
    }

    fn pending_event_jsons(db: &MetricsDatabase) -> Vec<String> {
        let mut stmt = db
            .conn
            .prepare("SELECT event_json FROM metrics WHERE delivered_ts IS NULL ORDER BY id DESC")
            .unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
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
        assert_eq!(version, "3");

        for column in [
            "delivered_ts",
            "attempts",
            "last_sync_error",
            "last_sync_at",
            "next_retry_at",
            "processing_started_at",
        ] {
            let column_count: i64 = db
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('metrics') WHERE name = ?1",
                    params![column],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(column_count, 1, "missing column {column}");
        }
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
            CREATE TABLE metrics (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_json TEXT NOT NULL
            );
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
        assert_eq!(version, "3");
    }

    #[test]
    fn test_migrates_version_2_to_row_level_retry_schema() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("v2.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE schema_metadata (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            INSERT INTO schema_metadata (key, value) VALUES ('version', '2');
            CREATE TABLE metrics (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_json TEXT NOT NULL
            );
            INSERT INTO metrics (event_json) VALUES ('{"t":1,"e":1,"v":{},"a":{}}');
            CREATE TABLE agent_usage_throttle (
                prompt_id TEXT PRIMARY KEY,
                last_sent_ts INTEGER NOT NULL
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
        assert_eq!(version, "3");
        assert_eq!(db.count().unwrap(), 1);
        assert_eq!(db.count_retryable().unwrap(), 1);
    }

    #[test]
    fn test_migrates_version_2_with_preexisting_retry_columns() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("v2-partial-retry.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE schema_metadata (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            INSERT INTO schema_metadata (key, value) VALUES ('version', '2');
            CREATE TABLE metrics (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_json TEXT NOT NULL,
                delivered_ts INTEGER,
                attempts INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO metrics (event_json) VALUES ('{"t":1,"e":1,"v":{},"a":{}}');
            CREATE TABLE agent_usage_throttle (
                prompt_id TEXT PRIMARY KEY,
                last_sent_ts INTEGER NOT NULL
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
        assert_eq!(version, "3");

        for column in [
            "delivered_ts",
            "attempts",
            "last_sync_error",
            "last_sync_at",
            "next_retry_at",
            "processing_started_at",
        ] {
            assert!(db.column_exists("metrics", column).unwrap());
        }
        assert_eq!(db.count_retryable().unwrap(), 1);
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
        assert_eq!(db.count_retryable().unwrap(), 2);
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn test_dequeue_pending_batch_locks_rows() {
        let (mut db, _temp_dir) = create_test_db();
        let events = vec![event_json(days_ago(2)), event_json(days_ago(1))];
        db.insert_events(&events).unwrap();

        let batch = db.dequeue_pending_batch(1).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(db.count().unwrap(), 2);
        assert_eq!(db.count_retryable().unwrap(), 1);

        db.mark_records_delivered(&[batch[0].id], unix_now())
            .unwrap();
        assert_eq!(db.count().unwrap(), 1);
        assert_eq!(db.count_retryable().unwrap(), 1);
    }

    #[test]
    fn test_dequeue_pending_batch_prefers_newest_retryable_rows() {
        let (mut db, _temp_dir) = create_test_db();
        let oldest_ts = days_ago(3);
        let middle_ts = days_ago(2);
        let newest_ts = days_ago(1);
        db.insert_events(&[
            event_json(oldest_ts),
            event_json(middle_ts),
            event_json(newest_ts),
        ])
        .unwrap();

        let batch = db.dequeue_pending_batch(2).unwrap();
        assert_eq!(batch.len(), 2);
        assert!(batch[0].id > batch[1].id);
        assert!(batch[0].event_json.contains(&format!("\"t\":{newest_ts}")));
        assert!(batch[1].event_json.contains(&format!("\"t\":{middle_ts}")));
    }

    #[test]
    fn test_failed_records_do_not_block_unfailed_retryable_rows() {
        let (mut db, _temp_dir) = create_test_db();
        db.insert_events(&[event_json(days_ago(2)), event_json(days_ago(1))])
            .unwrap();

        let batch = db.dequeue_pending_batch(1).unwrap();
        let failed_id = batch[0].id;
        let failed_at = unix_now();
        db.mark_records_failed(&[failed_id], "upload failed", failed_at)
            .unwrap();

        assert_eq!(db.count().unwrap(), 2);
        assert_eq!(db.count_retryable().unwrap(), 1);

        let retryable_batch = db.dequeue_pending_batch(10).unwrap();
        assert_eq!(retryable_batch.len(), 1);
        assert_ne!(retryable_batch[0].id, failed_id);

        let (attempts, next_retry_at): (i64, i64) = db
            .conn
            .query_row(
                "SELECT attempts, next_retry_at FROM metrics WHERE id = ?1",
                params![failed_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(attempts, 1);
        assert!(next_retry_at > failed_at as i64);
    }

    #[test]
    fn test_dequeue_releases_stale_processing_locks() {
        let (mut db, _temp_dir) = create_test_db();
        db.insert_events(&[event_json(days_ago(1))]).unwrap();

        let first_batch = db.dequeue_pending_batch(1).unwrap();
        assert_eq!(first_batch.len(), 1);
        assert_eq!(db.count_retryable().unwrap(), 0);

        let stale_started_at = unix_now().saturating_sub(METRIC_PROCESSING_LOCK_TIMEOUT_SECS + 1);
        db.conn
            .execute(
                "UPDATE metrics SET processing_started_at = ?1 WHERE id = ?2",
                params![stale_started_at as i64, first_batch[0].id],
            )
            .unwrap();

        let second_batch = db.dequeue_pending_batch(1).unwrap();
        assert_eq!(second_batch.len(), 1);
        assert_eq!(second_batch[0].id, first_batch[0].id);
    }

    #[test]
    fn test_max_attempts_are_not_retryable() {
        let (mut db, _temp_dir) = create_test_db();
        let ids = db.insert_events(&[event_json(days_ago(1))]).unwrap();
        db.conn
            .execute(
                "UPDATE metrics SET attempts = ?1 WHERE id = ?2",
                params![MAX_METRIC_UPLOAD_ATTEMPTS as i64, ids[0]],
            )
            .unwrap();

        assert_eq!(db.count().unwrap(), 1);
        assert_eq!(db.count_retryable().unwrap(), 0);
        assert!(db.dequeue_pending_batch(1).unwrap().is_empty());
    }

    #[test]
    fn test_mark_records_undeliverable_keeps_history_without_retrying() {
        let (mut db, _temp_dir) = create_test_db();
        let event_ts = days_ago(1);
        let ids = db.insert_events(&[event_json(event_ts)]).unwrap();

        let batch = db.dequeue_pending_batch(1).unwrap();
        assert_eq!(batch.len(), 1);
        db.mark_records_undeliverable(&[(ids[0], "validation failed".to_string())], unix_now())
            .unwrap();

        assert_eq!(db.count().unwrap(), 1);
        assert_eq!(db.count_retryable().unwrap(), 0);
        assert!(db.dequeue_pending_batch(1).unwrap().is_empty());
        assert_eq!(db.get_metric_history(0, None, &[1]).unwrap().len(), 1);

        let (delivered_ts, attempts, last_sync_error): (Option<i64>, i64, Option<String>) = db
            .conn
            .query_row(
                "SELECT delivered_ts, attempts, last_sync_error FROM metrics WHERE id = ?1",
                params![ids[0]],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert!(delivered_ts.is_none());
        assert_eq!(attempts, MAX_METRIC_UPLOAD_ATTEMPTS as i64);
        assert_eq!(last_sync_error.as_deref(), Some("validation failed"));
    }

    #[test]
    fn test_mark_records_delivered() {
        let (mut db, _temp_dir) = create_test_db();
        let ts1 = days_ago(3);
        let ts2 = days_ago(2);
        let ts3 = days_ago(1);

        let events = vec![event_json(ts1), event_json(ts2), event_json(ts3)];

        db.insert_events(&events).unwrap();

        // Dequeue newest rows and mark them delivered.
        let batch = db.dequeue_pending_batch(2).unwrap();
        let ids: Vec<i64> = batch.iter().map(|r| r.id).collect();

        db.mark_records_delivered(&ids, unix_now()).unwrap();

        // Verify only one remains pending.
        let count = db.count().unwrap();
        assert_eq!(count, 1);

        // Verify remaining pending row is the oldest one.
        let remaining = pending_event_jsons(&db);
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0].contains(&format!("\"t\":{ts1}")));

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

        let batch = pending_event_jsons(&db);
        assert_eq!(batch.len(), 1);
        assert!(batch[0].contains(&format!("\"t\":{pending_event_ts}")));
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
        assert_eq!(db.count().unwrap(), 3);
    }

    #[test]
    fn test_prunes_metric_rows_older_than_retention_by_event_timestamp() {
        let (mut db, _temp_dir) = create_test_db();

        let delivered_ts = unix_now();
        let old_event_ts = seconds_ago(MetricsDatabase::METRICS_RETENTION_SECS + 1);
        let recent_event_ts = seconds_ago(MetricsDatabase::METRICS_RETENTION_SECS - 1);
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

        let old_event_ts = seconds_ago(MetricsDatabase::METRICS_RETENTION_SECS + 1);
        let recent_event_ts = days_ago(1);
        let pending = vec![event_json(old_event_ts), event_json(recent_event_ts)];

        db.insert_events(&pending).unwrap();

        let total: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(db.count().unwrap(), 1);

        let batch = pending_event_jsons(&db);
        assert_eq!(batch.len(), 1);
        assert!(batch[0].contains(&format!("\"t\":{recent_event_ts}")));
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

        // Dequeue from empty should return empty.
        let batch = db.dequeue_pending_batch(10).unwrap();
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
