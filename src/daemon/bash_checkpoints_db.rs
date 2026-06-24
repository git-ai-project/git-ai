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

    // ----- Write operations -----

    /// Record (or update) a bash checkpoint's pre-hook facts. Upserts on the
    /// `(session_id, tool_use_id)` correlation pair so a duplicate pre-hook does
    /// not create a second row. A non-`None` command never overwrites a stored
    /// command with `NULL`.
    #[allow(clippy::too_many_arguments)]
    pub fn record_start(
        &mut self,
        session_id: &str,
        tool_use_id: &str,
        repo_work_dir: &str,
        agent: &crate::authorship::working_log::AgentId,
        command: Option<&str>,
        start_ns: i64,
        now_secs: i64,
    ) -> Result<(), GitAiError> {
        self.conn.execute(
            "INSERT INTO bash_checkpoints
                (session_id, tool_use_id, repo_work_dir, tool, agent_model, agent_internal_id, command, start_ns, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(session_id, tool_use_id) DO UPDATE SET
                repo_work_dir     = excluded.repo_work_dir,
                tool              = excluded.tool,
                agent_model       = excluded.agent_model,
                agent_internal_id = excluded.agent_internal_id,
                command           = COALESCE(excluded.command, bash_checkpoints.command),
                start_ns          = excluded.start_ns",
            params![
                session_id,
                tool_use_id,
                repo_work_dir,
                agent.tool,
                agent.model,
                agent.id,
                command,
                start_ns,
                now_secs
            ],
        )?;
        Ok(())
    }

    /// Record a bash checkpoint's post-hook end timestamp.
    pub fn record_end(
        &mut self,
        session_id: &str,
        tool_use_id: &str,
        end_ns: i64,
    ) -> Result<(), GitAiError> {
        self.conn.execute(
            "UPDATE bash_checkpoints SET end_ns = ?1 WHERE session_id = ?2 AND tool_use_id = ?3",
            params![end_ns, session_id, tool_use_id],
        )?;
        Ok(())
    }

    // ----- Read operations -----

    /// Return bash checkpoints for `repo_work_dir` whose `[start_ns, end_ns]`
    /// interval overlaps the query window `[window_lo_ns, window_hi_ns]`. A NULL
    /// `end_ns` is treated as a point at `start_ns`.
    pub fn find_candidates(
        &self,
        repo_work_dir: &str,
        window_lo_ns: i64,
        window_hi_ns: i64,
    ) -> Result<Vec<BashCheckpointRow>, GitAiError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, tool_use_id, tool, agent_internal_id, agent_model, command, start_ns, end_ns
             FROM bash_checkpoints
             WHERE repo_work_dir = ?1
               AND start_ns <= ?3
               AND COALESCE(end_ns, start_ns) >= ?2
             ORDER BY start_ns",
        )?;
        let rows = stmt.query_map(params![repo_work_dir, window_lo_ns, window_hi_ns], |r| {
            Ok(BashCheckpointRow {
                session_id: r.get(0)?,
                tool_use_id: r.get(1)?,
                tool: r.get(2)?,
                agent_internal_id: r.get(3)?,
                agent_model: r.get(4)?,
                command: r.get(5)?,
                start_ns: r.get(6)?,
                end_ns: r.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Delete rows older than the 30-day retention window. Rate-limited to one
    /// pass per 24h via the `bash_ckpt_last_prune_ts` metadata cursor. Returns
    /// the number of rows deleted (0 when the pass is skipped).
    pub fn prune_old(&mut self, now_secs: i64) -> Result<usize, GitAiError> {
        const PRUNE_INTERVAL_SECS: i64 = 86_400;
        let last: i64 = self
            .conn
            .query_row(
                "SELECT value FROM schema_metadata WHERE key = 'bash_ckpt_last_prune_ts'",
                [],
                |r| {
                    let v: String = r.get(0)?;
                    Ok(v.parse::<i64>().unwrap_or(0))
                },
            )
            .unwrap_or(0);
        if now_secs - last < PRUNE_INTERVAL_SECS {
            return Ok(0);
        }
        let cutoff = now_secs - BASH_CKPT_RETENTION_SECS;
        let deleted = self.conn.execute(
            "DELETE FROM bash_checkpoints WHERE created_at < ?1",
            params![cutoff],
        )?;
        self.conn.execute(
            "INSERT INTO schema_metadata (key, value) VALUES ('bash_ckpt_last_prune_ts', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![now_secs.to_string()],
        )?;
        Ok(deleted)
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

    use crate::authorship::working_log::AgentId;

    fn agent() -> AgentId {
        AgentId {
            tool: "claude".into(),
            id: "sess1".into(),
            model: "opus".into(),
        }
    }

    #[test]
    fn test_record_start_then_end_roundtrips() {
        let (mut db, _t) = test_db();
        db.record_start("s1", "t1", "/repo", &agent(), Some("ls -la"), 1_000, 10)
            .unwrap();
        db.record_end("s1", "t1", 2_000).unwrap();
        let row: (String, Option<i64>, Option<String>) = db
            .conn
            .query_row(
                "SELECT command, end_ns, tool FROM bash_checkpoints WHERE session_id='s1' AND tool_use_id='t1'",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row.0, "ls -la");
        assert_eq!(row.1, Some(2_000));
        assert_eq!(row.2, Some("claude".to_string()));
    }

    #[test]
    fn test_record_start_upsert_no_dup() {
        let (mut db, _t) = test_db();
        db.record_start("s1", "t1", "/repo", &agent(), None, 1_000, 10)
            .unwrap();
        db.record_start("s1", "t1", "/repo", &agent(), Some("cmd"), 1_500, 10)
            .unwrap();
        let n: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM bash_checkpoints WHERE session_id='s1' AND tool_use_id='t1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    #[serial_test::serial(bash_ckpt_env)]
    fn test_database_path_honors_env_override() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("g.db");
        unsafe {
            std::env::set_var("GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH", &path);
        }
        let resolved = BashCheckpointsDatabase::database_path().unwrap();
        unsafe {
            std::env::remove_var("GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH");
        }
        assert_eq!(resolved, path);
    }

    #[test]
    fn test_find_candidates_window_and_repo() {
        let (mut db, _t) = test_db();
        db.record_start("s1", "t1", "/repo", &agent(), None, 1_000, 10)
            .unwrap();
        db.record_end("s1", "t1", 2_000).unwrap();
        db.record_start("s2", "t2", "/repo", &agent(), None, 50_000, 10)
            .unwrap();
        db.record_start("s3", "t3", "/other", &agent(), None, 1_500, 10)
            .unwrap();

        let hits = db.find_candidates("/repo", 1_800, 2_200).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].tool_use_id, "t1");
    }

    #[test]
    fn test_find_candidates_null_end_is_point() {
        let (mut db, _t) = test_db();
        db.record_start("s1", "t1", "/repo", &agent(), None, 5_000, 10)
            .unwrap();
        assert_eq!(db.find_candidates("/repo", 4_000, 6_000).unwrap().len(), 1);
        assert_eq!(db.find_candidates("/repo", 6_001, 7_000).unwrap().len(), 0);
    }

    #[test]
    fn test_prune_old_removes_expired() {
        let (mut db, _t) = test_db();
        let now = 100 * 86_400;
        db.record_start("old", "t", "/repo", &agent(), None, 1, now - 40 * 86_400)
            .unwrap();
        db.record_start("new", "t", "/repo", &agent(), None, 1, now - 1)
            .unwrap();
        let deleted = db.prune_old(now).unwrap();
        assert_eq!(deleted, 1);
        let n: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM bash_checkpoints", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn test_prune_rate_limited() {
        let (mut db, _t) = test_db();
        let now = 100 * 86_400;
        db.record_start("old", "t", "/repo", &agent(), None, 1, now - 40 * 86_400)
            .unwrap();
        assert_eq!(db.prune_old(now).unwrap(), 1);
        db.record_start("old2", "t", "/repo", &agent(), None, 1, now - 40 * 86_400)
            .unwrap();
        assert_eq!(
            db.prune_old(now).unwrap(),
            0,
            "second prune within 24h is skipped"
        );
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
