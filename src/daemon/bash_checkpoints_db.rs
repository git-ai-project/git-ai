//! Dedicated bash-checkpoint storage at `~/.git-ai/internal/bash-checkpoints-db`.
//!
//! Persists one row per bash tool-call (pre/post correlated by
//! `(session_id, tool_use_id)`) so that post-commit attribution recovery can
//! correlate a committed file's mtime/ctime with the shell command that most
//! likely produced it. Rows are pruned after 30 days.
//!
//! This database is SEPARATE from every other git-ai SQLite store. It follows
//! the same skeleton as `src/notes/db.rs`: an `OnceLock<Mutex<_>>` singleton via
//! `global()`, `open_at_path()` for isolated test instances, a
//! `GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH` env override, WAL pragmas, and a
//! versioned `MIGRATIONS` array tracked by `schema_metadata(key, value)`.

use crate::error::GitAiError;
use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Current schema version (must equal MIGRATIONS.len()).
const SCHEMA_VERSION: usize = 1;

/// Rows older than this (by `created_at`) are pruned.
pub const BASH_CKPT_RETENTION_SECS: i64 = 30 * 86_400;

/// Database migrations — each entry upgrades the schema by one version.
const MIGRATIONS: &[&str] = &[
    // Migration 0 → 1: one row per bash tool-call.
    r#"
    CREATE TABLE IF NOT EXISTS bash_checkpoints (
        id                  INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id          TEXT NOT NULL,
        tool_use_id         TEXT NOT NULL,
        repo_work_dir       TEXT NOT NULL,
        tool                TEXT NOT NULL,
        agent_model         TEXT,
        agent_internal_id   TEXT,
        command             TEXT,
        start_ns            INTEGER NOT NULL,
        end_ns              INTEGER,
        created_at          INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_bash_ckpt_repo_time
        ON bash_checkpoints(repo_work_dir, start_ns);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_bash_ckpt_corr
        ON bash_checkpoints(session_id, tool_use_id);
    "#,
];

/// Global singleton for the bash-checkpoints database.
static BASH_CKPT_DB: OnceLock<Mutex<BashCheckpointsDatabase>> = OnceLock::new();

/// A bash-checkpoint row returned from `find_candidates`.
#[derive(Debug, Clone)]
pub struct BashCheckpointRow {
    pub session_id: String,
    pub tool_use_id: String,
    pub tool: String,
    pub agent_internal_id: Option<String>,
    pub agent_model: Option<String>,
    pub command: Option<String>,
    pub start_ns: i64,
    pub end_ns: Option<i64>,
}

/// SQLite wrapper for bash-checkpoint storage.
pub struct BashCheckpointsDatabase {
    conn: Connection,
}

impl BashCheckpointsDatabase {
    /// Return (or lazily initialize) the global database mutex.
    pub fn global() -> Result<&'static Mutex<BashCheckpointsDatabase>, GitAiError> {
        let db = BASH_CKPT_DB.get_or_init(|| match Self::new() {
            Ok(db) => Mutex::new(db),
            Err(e) => {
                eprintln!(
                    "[Error] Failed to initialize bash-checkpoints database: {}",
                    e
                );
                let temp_path = std::env::temp_dir().join("git-ai-bash-ckpt-db-failed");
                let conn = Connection::open(&temp_path).expect("Failed to create temp DB");
                Mutex::new(BashCheckpointsDatabase { conn })
            }
        });
        Ok(db)
    }

    /// Open a database at an explicit path (isolated test instances).
    pub fn open_at_path(path: &std::path::Path) -> Result<Self, GitAiError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-2000; PRAGMA temp_store=MEMORY;",
        )?;
        let mut db = Self { conn };
        db.initialize_schema()?;
        Ok(db)
    }

    /// Open (or create) the database at the configured path.
    fn new() -> Result<Self, GitAiError> {
        let db_path = Self::database_path()?;
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-2000; PRAGMA temp_store=MEMORY;",
        )?;
        let mut db = Self { conn };
        db.initialize_schema()?;
        Ok(db)
    }

    /// Resolve the on-disk path; honor the test override env var.
    fn database_path() -> Result<PathBuf, GitAiError> {
        #[cfg(any(test, feature = "test-support"))]
        if let Ok(test_path) = std::env::var("GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH") {
            return Ok(PathBuf::from(test_path));
        }
        let home = dirs::home_dir()
            .ok_or_else(|| GitAiError::Generic("Could not determine home directory".to_string()))?;
        Ok(home
            .join(".git-ai")
            .join("internal")
            .join("bash-checkpoints-db"))
    }

    /// Apply schema migrations until the DB is at `SCHEMA_VERSION`.
    fn initialize_schema(&mut self) -> Result<(), GitAiError> {
        let version_check: Result<usize, _> = self.conn.query_row(
            "SELECT value FROM schema_metadata WHERE key = 'version'",
            [],
            |row| {
                let v: String = row.get(0)?;
                v.parse::<usize>()
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            },
        );
        if let Ok(current) = version_check {
            if current == SCHEMA_VERSION {
                return Ok(());
            }
            if current > SCHEMA_VERSION {
                return Err(GitAiError::Generic(format!(
                    "Bash-checkpoints DB schema version {} newer than supported {}. Upgrade git-ai.",
                    current, SCHEMA_VERSION
                )));
            }
        }
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_metadata (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);",
        )?;
        let current: usize = self
            .conn
            .query_row(
                "SELECT value FROM schema_metadata WHERE key = 'version'",
                [],
                |row| {
                    let v: String = row.get(0)?;
                    v.parse::<usize>()
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
                },
            )
            .unwrap_or(0);
        for target in current..SCHEMA_VERSION {
            let tx = self.conn.transaction()?;
            tx.execute_batch(MIGRATIONS[target])?;
            tx.commit()?;
            self.conn.execute(
                "INSERT INTO schema_metadata (key, value) VALUES ('version', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value
                 WHERE CAST(schema_metadata.value AS INTEGER) < CAST(excluded.value AS INTEGER)",
                params![(target + 1).to_string()],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_db() -> (BashCheckpointsDatabase, TempDir) {
        let dir = TempDir::new().unwrap();
        let db = BashCheckpointsDatabase::open_at_path(&dir.path().join("bash-ckpt.db")).unwrap();
        (db, dir)
    }

    #[test]
    fn test_fresh_db_creates_table_and_version() {
        let (db, _t) = test_db();
        let n: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='bash_checkpoints'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        let v: String = db
            .conn
            .query_row(
                "SELECT value FROM schema_metadata WHERE key='version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, "1");
    }
}
