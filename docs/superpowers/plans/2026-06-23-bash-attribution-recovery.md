# Bash Attribution Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Recover AI attribution for shell/bash-produced changes that currently land as untracked, by persisting bash-checkpoint facts in a daemon-managed SQLite DB and adding a post-commit attribution-recovery solver pipeline.

**Architecture:** A new daemon-owned SQLite DB (`bash-checkpoints-db`, 30-day retention) is written from the existing `BashSessionStart`/`BashSessionEnd` control-socket handlers. During post-commit finalization, an `AttributionRecovery` orchestrator runs ordered `RecoverySolver`s over files that still have unknown lines: Solver 1 correlates file mtime/ctime to a bash checkpoint (±3s) and covers the file's unknown lines to that session; Solver 2 extends AI attribution into directly-adjacent unknown line runs. Each recovery emits a checkpoint metric event with a new `attribution_recovery_metadata` JSON field.

**Tech Stack:** Rust 2024, rusqlite (WAL), serde, the existing daemon control socket and `crate::metrics` machinery.

## Global Constraints

- Rust 2024 edition, Rust 1.93.0 (let-chains allowed).
- Git CLI only; no libgit2. All DB writes happen daemon-side; never block the checkpoint hot path.
- No backwards compat required on the daemon control socket (daemon + CLI update atomically).
- Paths POSIX-normalized via `normalize_to_posix()`; working/authorship logs use forward slashes.
- New SQLite DB MUST follow the `src/notes/db.rs` pattern: `OnceLock<Mutex<Db>>` singleton + `global()`, `open_at_path()` for tests, `database_path()` honoring a `GIT_AI_TEST_*_DB_PATH` env override, WAL pragmas, versioned `MIGRATIONS: &[&str]` with `schema_metadata(key,value)` version row.
- Retention reuses the metrics-db rate-limited prune pattern (≥24h between passes).
- Tests run debug builds. Use `task test TEST_FILTER=foo`. Always assert line-level attribution after EVERY commit in integration tests (`assert_committed_lines` / `assert_lines_and_blame`).
- Feature flags via `define_feature_flags!` in `src/feature_flags.rs`.
- Reuse, do not duplicate: `StatEntry::from_metadata`, `generate_session_id`, `generate_trace_id`, `AttestationEntry`, `SessionRecord`, `LineRange::compress_lines`, `CheckpointValues`, `crate::metrics::record`.

---

## File Structure

**Created:**
- `src/daemon/bash_checkpoints_db.rs` — the new SQLite DB (schema, CRUD, retention).
- `src/authorship/recovery/mod.rs` — orchestrator, `RecoverySolver` trait, shared types, `unknown_lines` helper, metric emission.
- `src/authorship/recovery/bash_solver.rs` — Solver 1 (bash mtime/ctime correlation).
- `src/authorship/recovery/edge_solver.rs` — Solver 2 (AI edge extension).

**Modified:**
- `src/daemon.rs` — register DB; persist from `BashSessionStart`/`BashSessionEnd`; call retention.
- `src/daemon/control_api.rs` — extend `BashSessionStart`/`BashSessionEnd` with command + ns timestamps.
- `src/commands/checkpoint_agent/presets/mod.rs` — add `command` to `PreBashCall`/`PostBashCall`.
- `src/commands/checkpoint_agent/presets/parse.rs` — `bash_command_from_tool_input` helper.
- `src/commands/checkpoint_agent/presets/{claude,cursor,gemini,codex,...}.rs` — populate command.
- `src/commands/checkpoint_agent/orchestrator.rs` — stamp ns + thread command into control msgs.
- `src/authorship/mod.rs` — `pub mod recovery;`.
- `src/authorship/background_agent.rs` — extract shared `unknown_lines` helper (DRY).
- `src/authorship/post_commit.rs` — hoist `committed_hunks`; call `recover_attribution`; emit metrics.
- `src/metrics/events.rs` — add `attribution_recovery_metadata` field to `CheckpointValues`.
- `src/feature_flags.rs` — `attribution_recovery`, `bash_checkpoint_tracking` flags.
- `tests/integration/repos/test_repo.rs` + a `mock_bash` checkpoint mock — integration helpers.

---

## Task 1: New metric field `attribution_recovery_metadata`

**Files:**
- Modify: `src/metrics/events.rs` (checkpoint_pos ~445-455; `CheckpointValues` ~474-485; builders ~580-590; `to_sparse`/`from_sparse` ~592-660)
- Test: `src/metrics/events.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces: `CheckpointValues::attribution_recovery_metadata(impl Into<String>) -> Self`; new const `checkpoint_pos::ATTRIBUTION_RECOVERY_METADATA = 9`.

- [ ] **Step 1: Write the failing test** — append to the events.rs test module:

```rust
#[test]
fn test_checkpoint_attribution_recovery_metadata_roundtrips() {
    let json = r#"{"solver":"bash_correlation","delta_ns":12345}"#;
    let values = CheckpointValues::new()
        .kind("ai_agent")
        .edit_kind("bash")
        .attribution_recovery_metadata(json);
    let sparse = values.to_sparse();
    let restored = CheckpointValues::from_sparse(&sparse);
    assert_eq!(
        restored.attribution_recovery_metadata,
        Some(Some(json.to_string()))
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_checkpoint_attribution_recovery_metadata_roundtrips`
Expected: FAIL — no method `attribution_recovery_metadata` / field missing.

- [ ] **Step 3: Implement**

In `checkpoint_pos` add after `EDIT_KIND`:
```rust
    pub const ATTRIBUTION_RECOVERY_METADATA: usize = 9; // String (nullable) JSON
```
Add field to `CheckpointValues` after `edit_kind`:
```rust
    pub attribution_recovery_metadata: PosField<String>,
```
Add builders after `edit_kind`/`edit_kind_null`:
```rust
    pub fn attribution_recovery_metadata(mut self, value: impl Into<String>) -> Self {
        self.attribution_recovery_metadata = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn attribution_recovery_metadata_null(mut self) -> Self {
        self.attribution_recovery_metadata = Some(None);
        self
    }
```
In `to_sparse` add after the `EDIT_KIND` `sparse_set`:
```rust
        sparse_set(
            &mut map,
            checkpoint_pos::ATTRIBUTION_RECOVERY_METADATA,
            string_to_json(&self.attribution_recovery_metadata),
        );
```
In `from_sparse` add the field:
```rust
            attribution_recovery_metadata: sparse_get_string(
                arr,
                checkpoint_pos::ATTRIBUTION_RECOVERY_METADATA,
            ),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `task test TEST_FILTER=test_checkpoint_attribution_recovery_metadata_roundtrips`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/metrics/events.rs
git commit -m "feat(metrics): add attribution_recovery_metadata checkpoint field"
```

---

## Task 2: Feature flags

**Files:**
- Modify: `src/feature_flags.rs:80-85` and the test asserts at ~135-195.

**Interfaces:**
- Produces: `FeatureFlags { attribution_recovery: bool, bash_checkpoint_tracking: bool }`.

- [ ] **Step 1: Write the failing test** — in the `tests` module, extend `test_default_feature_flags` debug + release blocks:

```rust
        assert!(flags.attribution_recovery);
        assert!(flags.bash_checkpoint_tracking);
```
(Add to both the `#[cfg(debug_assertions)]` and `#[cfg(not(debug_assertions))]` blocks.)

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_default_feature_flags`
Expected: FAIL — unknown field.

- [ ] **Step 3: Implement** — add to the `define_feature_flags!` invocation:

```rust
    attribution_recovery: attribution_recovery, debug = true, release = true,
    bash_checkpoint_tracking: bash_checkpoint_tracking, debug = true, release = true,
```
Also update any hand-written `FeatureFlags { ... }` literal in the test module (~line 191) to include `attribution_recovery: true, bash_checkpoint_tracking: true,`.

- [ ] **Step 4: Run test to verify it passes**

Run: `task test TEST_FILTER=feature_flags`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/feature_flags.rs
git commit -m "feat(flags): add attribution_recovery + bash_checkpoint_tracking"
```

---

## Task 3: `bash_checkpoints_db` schema + open/migrate

**Files:**
- Create: `src/daemon/bash_checkpoints_db.rs`
- Modify: `src/daemon.rs` (add `pub mod bash_checkpoints_db;` near `pub mod bash_sessions;` at ~line 49)

**Interfaces:**
- Produces: `BashCheckpointsDatabase` with `open_at_path(&Path) -> Result<Self, GitAiError>`, `global() -> Result<&'static Mutex<Self>, GitAiError>`, `database_path() -> Result<PathBuf, GitAiError>`, `initialize_schema`. Schema version 1.

- [ ] **Step 1: Write the failing test** — create the file with module skeleton + first test:

```rust
//! Dedicated bash-checkpoint storage at `~/.git-ai/internal/bash-checkpoints-db`.
//!
//! Persists one row per bash tool-call (pre/post correlated by
//! `(session_id, tool_use_id)`) so that post-commit attribution recovery can
//! correlate a committed file's mtime/ctime with the shell command that most
//! likely produced it. Rows are pruned after 30 days.

use crate::error::GitAiError;
use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const SCHEMA_VERSION: usize = 1;
pub const BASH_CKPT_RETENTION_SECS: i64 = 30 * 86_400;

const MIGRATIONS: &[&str] = &[
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

static BASH_CKPT_DB: OnceLock<Mutex<BashCheckpointsDatabase>> = OnceLock::new();

pub struct BashCheckpointsDatabase {
    conn: Connection,
}

impl BashCheckpointsDatabase {
    pub fn global() -> Result<&'static Mutex<BashCheckpointsDatabase>, GitAiError> {
        let db = BASH_CKPT_DB.get_or_init(|| match Self::new() {
            Ok(db) => Mutex::new(db),
            Err(e) => {
                eprintln!("[Error] Failed to initialize bash-checkpoints database: {}", e);
                let temp_path = std::env::temp_dir().join("git-ai-bash-ckpt-db-failed");
                let conn = Connection::open(&temp_path).expect("Failed to create temp DB");
                Mutex::new(BashCheckpointsDatabase { conn })
            }
        });
        Ok(db)
    }

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

    fn database_path() -> Result<PathBuf, GitAiError> {
        #[cfg(any(test, feature = "test-support"))]
        if let Ok(test_path) = std::env::var("GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH") {
            return Ok(PathBuf::from(test_path));
        }
        let home = dirs::home_dir()
            .ok_or_else(|| GitAiError::Generic("Could not determine home directory".to_string()))?;
        Ok(home.join(".git-ai").join("internal").join("bash-checkpoints-db"))
    }

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
        let n: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='bash_checkpoints'",
            [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let v: String = db.conn.query_row(
            "SELECT value FROM schema_metadata WHERE key='version'", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "1");
    }
}
```
Add `pub mod bash_checkpoints_db;` in `src/daemon.rs` next to `pub mod bash_sessions;`.

- [ ] **Step 2: Run test to verify it fails (then compiles+passes once module is wired)**

Run: `task test TEST_FILTER=test_fresh_db_creates_table_and_version`
Expected: initially FAIL to compile until `pub mod` added; then PASS.

- [ ] **Step 3: Implement** — already included above; ensure `pub mod` line present.

- [ ] **Step 4: Run test to verify it passes**

Run: `task test TEST_FILTER=bash_checkpoints_db`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/bash_checkpoints_db.rs src/daemon.rs
git commit -m "feat(daemon): bash-checkpoints SQLite DB schema + open/migrate"
```

---

## Task 4: `bash_checkpoints_db` CRUD — record_start / record_end

**Files:**
- Modify: `src/daemon/bash_checkpoints_db.rs`

**Interfaces:**
- Consumes: `crate::authorship::working_log::AgentId`.
- Produces:
  - `record_start(&mut self, session_id: &str, tool_use_id: &str, repo_work_dir: &str, agent: &AgentId, command: Option<&str>, start_ns: i64, now_secs: i64) -> Result<(), GitAiError>`
  - `record_end(&mut self, session_id: &str, tool_use_id: &str, end_ns: i64) -> Result<(), GitAiError>`
  - `pub struct BashCheckpointRow { pub session_id: String, pub tool_use_id: String, pub tool: String, pub agent_internal_id: Option<String>, pub agent_model: Option<String>, pub command: Option<String>, pub start_ns: i64, pub end_ns: Option<i64> }`

- [ ] **Step 1: Write the failing test**

```rust
    use crate::authorship::working_log::AgentId;

    fn agent() -> AgentId {
        AgentId { tool: "claude".into(), id: "sess1".into(), model: "opus".into() }
    }

    #[test]
    fn test_record_start_then_end_roundtrips() {
        let (mut db, _t) = test_db();
        db.record_start("s1", "t1", "/repo", &agent(), Some("ls -la"), 1_000, 10).unwrap();
        db.record_end("s1", "t1", 2_000).unwrap();
        let row: (String, Option<i64>, Option<String>) = db.conn.query_row(
            "SELECT command, end_ns, tool FROM bash_checkpoints WHERE session_id='s1' AND tool_use_id='t1'",
            [], |r| Ok((r.get::<_,String>(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(row.0, "ls -la");
        assert_eq!(row.1, Some(2_000));
        assert_eq!(row.2, Some("claude".to_string()));
    }

    #[test]
    fn test_record_start_upsert_no_dup() {
        let (mut db, _t) = test_db();
        db.record_start("s1", "t1", "/repo", &agent(), None, 1_000, 10).unwrap();
        db.record_start("s1", "t1", "/repo", &agent(), Some("cmd"), 1_500, 10).unwrap();
        let n: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM bash_checkpoints WHERE session_id='s1' AND tool_use_id='t1'",
            [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_record_start_then_end_roundtrips`
Expected: FAIL — no method `record_start`.

- [ ] **Step 3: Implement** — add to `impl BashCheckpointsDatabase`:

```rust
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
                repo_work_dir = excluded.repo_work_dir,
                tool          = excluded.tool,
                agent_model   = excluded.agent_model,
                agent_internal_id = excluded.agent_internal_id,
                command       = COALESCE(excluded.command, bash_checkpoints.command),
                start_ns      = excluded.start_ns",
            params![
                session_id, tool_use_id, repo_work_dir, agent.tool,
                agent.model, agent.id, command, start_ns, now_secs
            ],
        )?;
        Ok(())
    }

    pub fn record_end(&mut self, session_id: &str, tool_use_id: &str, end_ns: i64) -> Result<(), GitAiError> {
        self.conn.execute(
            "UPDATE bash_checkpoints SET end_ns = ?1 WHERE session_id = ?2 AND tool_use_id = ?3",
            params![end_ns, session_id, tool_use_id],
        )?;
        Ok(())
    }
```
Add the `BashCheckpointRow` struct above the impl (used in Task 5).

- [ ] **Step 4: Run test to verify it passes**

Run: `task test TEST_FILTER=bash_checkpoints_db`
Expected: PASS (both new tests).

- [ ] **Step 5: Commit**

```bash
git add src/daemon/bash_checkpoints_db.rs
git commit -m "feat(daemon): bash-checkpoints record_start/record_end"
```

---

## Task 5: `bash_checkpoints_db` query + retention

**Files:**
- Modify: `src/daemon/bash_checkpoints_db.rs`

**Interfaces:**
- Produces:
  - `find_candidates(&self, repo_work_dir: &str, window_lo_ns: i64, window_hi_ns: i64) -> Result<Vec<BashCheckpointRow>, GitAiError>` — rows whose `[start_ns, COALESCE(end_ns,start_ns)]` interval overlaps `[lo,hi]`, same repo.
  - `prune_old(&mut self, now_secs: i64) -> Result<usize, GitAiError>` — deletes rows with `created_at < now - 30d`, rate-limited ≥24h via `schema_metadata` key `bash_ckpt_last_prune_ts`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn test_find_candidates_window_and_repo() {
        let (mut db, _t) = test_db();
        // in window, right repo
        db.record_start("s1", "t1", "/repo", &agent(), None, 1_000, 10).unwrap();
        db.record_end("s1", "t1", 2_000).unwrap();
        // out of window
        db.record_start("s2", "t2", "/repo", &agent(), None, 50_000, 10).unwrap();
        // other repo, in window
        db.record_start("s3", "t3", "/other", &agent(), None, 1_500, 10).unwrap();

        let hits = db.find_candidates("/repo", 1_800, 2_200).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].tool_use_id, "t1");
    }

    #[test]
    fn test_find_candidates_null_end_is_point() {
        let (mut db, _t) = test_db();
        db.record_start("s1", "t1", "/repo", &agent(), None, 5_000, 10).unwrap(); // no end
        assert_eq!(db.find_candidates("/repo", 4_000, 6_000).unwrap().len(), 1);
        assert_eq!(db.find_candidates("/repo", 6_001, 7_000).unwrap().len(), 0);
    }

    #[test]
    fn test_prune_old_removes_expired() {
        let (mut db, _t) = test_db();
        let now = 100 * 86_400; // day 100
        db.record_start("old", "t", "/repo", &agent(), None, 1, now - 40 * 86_400).unwrap();
        db.record_start("new", "t", "/repo", &agent(), None, 1, now - 1).unwrap();
        let deleted = db.prune_old(now).unwrap();
        assert_eq!(deleted, 1);
        let n: i64 = db.conn.query_row("SELECT COUNT(*) FROM bash_checkpoints", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn test_prune_rate_limited() {
        let (mut db, _t) = test_db();
        let now = 100 * 86_400;
        db.record_start("old", "t", "/repo", &agent(), None, 1, now - 40 * 86_400).unwrap();
        assert_eq!(db.prune_old(now).unwrap(), 1);
        // immediate second prune is a no-op (rate limited); add another expired row first
        db.record_start("old2", "t", "/repo", &agent(), None, 1, now - 40 * 86_400).unwrap();
        assert_eq!(db.prune_old(now).unwrap(), 0, "second prune within 24h is skipped");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_find_candidates_window_and_repo`
Expected: FAIL — no method `find_candidates`.

- [ ] **Step 3: Implement**

```rust
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
        for row in rows { out.push(row?); }
        Ok(out)
    }

    pub fn prune_old(&mut self, now_secs: i64) -> Result<usize, GitAiError> {
        const PRUNE_INTERVAL_SECS: i64 = 86_400;
        let last: i64 = self.conn.query_row(
            "SELECT value FROM schema_metadata WHERE key = 'bash_ckpt_last_prune_ts'",
            [], |r| {
                let v: String = r.get(0)?;
                Ok(v.parse::<i64>().unwrap_or(0))
            }).unwrap_or(0);
        if now_secs - last < PRUNE_INTERVAL_SECS {
            return Ok(0);
        }
        let cutoff = now_secs - BASH_CKPT_RETENTION_SECS;
        let deleted = self.conn.execute(
            "DELETE FROM bash_checkpoints WHERE created_at < ?1", params![cutoff])?;
        self.conn.execute(
            "INSERT INTO schema_metadata (key, value) VALUES ('bash_ckpt_last_prune_ts', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![now_secs.to_string()])?;
        Ok(deleted)
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `task test TEST_FILTER=bash_checkpoints_db`
Expected: PASS (all DB tests).

- [ ] **Step 5: Commit**

```bash
git add src/daemon/bash_checkpoints_db.rs
git commit -m "feat(daemon): bash-checkpoints find_candidates + retention prune"
```

---

## Task 6: Control socket — carry command + timestamps

**Files:**
- Modify: `src/daemon/control_api.rs:29-42` (`BashSessionStart`, `BashSessionEnd`)
- Modify: `src/daemon.rs:5065-5091` (handlers) — persist to DB.

**Interfaces:**
- Consumes: `BashCheckpointsDatabase::{global, record_start, record_end, prune_old}` (Tasks 3-5).
- Produces: `BashSessionStart` gains `command: Option<String>`, `start_ns: i64`; `BashSessionEnd` gains `end_ns: i64`.

- [ ] **Step 1: Write the failing test** — add a daemon-side unit test in `src/daemon/bash_checkpoints_db.rs` is not possible (handler lives in daemon.rs). Instead add an integration assertion later (Task 12). For this task, the "test" is a compile + a focused handler unit test using the env-overridden DB. Add to `bash_checkpoints_db.rs` tests:

```rust
    #[test]
    #[serial_test::serial(bash_ckpt_env)]
    fn test_global_honors_env_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("g.db");
        unsafe { std::env::set_var("GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH", &path); }
        // global() initializes once; just assert path resolution
        let resolved = BashCheckpointsDatabase::database_path().unwrap();
        assert_eq!(resolved, path);
        unsafe { std::env::remove_var("GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH"); }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_global_honors_env_path`
Expected: PASS already if Task 3 done (this guards the env path). If it fails, fix `database_path`. (This task's real verification is `task build`.)

- [ ] **Step 3: Implement**

In `control_api.rs`:
```rust
    #[serde(rename = "bash_session.start")]
    BashSessionStart {
        repo_work_dir: String,
        session_id: String,
        tool_use_id: String,
        agent_id: AgentId,
        metadata: HashMap<String, String>,
        stat_snapshot: Box<StatSnapshot>,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        start_ns: i64,
    },
    #[serde(rename = "bash_session.end")]
    BashSessionEnd {
        session_id: String,
        tool_use_id: String,
        #[serde(default)]
        end_ns: i64,
    },
```
In `daemon.rs` `BashSessionStart` handler, after `state.start_session(...)`:
```rust
                if crate::config::Config::fresh().get_feature_flags().bash_checkpoint_tracking {
                    let repo_key = Self::worktree_state_key(Path::new(&repo_work_dir));
                    if let Ok(db) = crate::daemon::bash_checkpoints_db::BashCheckpointsDatabase::global() {
                        let now_secs = Self::unix_secs_now();
                        if let Ok(mut guard) = db.lock() {
                            let _ = guard.record_start(
                                &session_id_for_db, &tool_use_id_for_db, &repo_key,
                                &agent_id_for_db, command.as_deref(), start_ns, now_secs);
                            let _ = guard.prune_old(now_secs);
                        }
                    }
                }
```
(Bind `session_id_for_db`/`tool_use_id_for_db`/`agent_id_for_db` clones *before* moving them into `state.start_session`.) Add a small `fn unix_secs_now() -> i64` helper on the daemon impl, or inline `SystemTime::now()`. In the `BashSessionEnd` handler, after `state.end_session(...)`:
```rust
                if crate::config::Config::fresh().get_feature_flags().bash_checkpoint_tracking
                    && let Ok(db) = crate::daemon::bash_checkpoints_db::BashCheckpointsDatabase::global()
                    && let Ok(mut guard) = db.lock()
                {
                    let _ = guard.record_end(&session_id, &tool_use_id, end_ns);
                }
```
Verify `crate::feature_flags::flags()` is the accessor (grep; if it's `FeatureFlags::get()` use that).

- [ ] **Step 4: Verify build + test**

Run: `task build` then `task test TEST_FILTER=bash_checkpoints_db`
Expected: compiles; tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/control_api.rs src/daemon.rs
git commit -m "feat(daemon): persist bash checkpoints from control handlers"
```

---

## Task 7: Client — extract command + stamp ns

**Files:**
- Modify: `src/commands/checkpoint_agent/presets/parse.rs` — add helper.
- Modify: `src/commands/checkpoint_agent/presets/mod.rs` — add `command: Option<String>` to `PreBashCall`/`PostBashCall`.
- Modify: `src/commands/checkpoint_agent/presets/claude.rs` (and cursor/gemini/codex where bash command is in `tool_input.command`) — populate.
- Modify: `src/commands/checkpoint_agent/orchestrator.rs:397-488` and the `bash_tool` send sites — stamp `start_ns`/`end_ns` and pass `command` into the control messages.

**Interfaces:**
- Consumes: extended `BashSessionStart`/`BashSessionEnd` (Task 6).
- Produces: `parse::bash_command_from_tool_input(&serde_json::Value) -> Option<String>`.

- [ ] **Step 1: Write the failing test** — in `parse.rs` test module:

```rust
#[test]
fn test_bash_command_from_tool_input() {
    let v = serde_json::json!({"tool_input": {"command": "cargo test"}});
    assert_eq!(super::bash_command_from_tool_input(&v), Some("cargo test".to_string()));
    let v2 = serde_json::json!({"toolInput": {"command": "ls"}});
    assert_eq!(super::bash_command_from_tool_input(&v2), Some("ls".to_string()));
    let v3 = serde_json::json!({"tool_input": {}});
    assert_eq!(super::bash_command_from_tool_input(&v3), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_bash_command_from_tool_input`
Expected: FAIL — function missing.

- [ ] **Step 3: Implement**

In `parse.rs`:
```rust
pub fn bash_command_from_tool_input(data: &Value) -> Option<String> {
    let ti = data.get("tool_input").or_else(|| data.get("toolInput"))?;
    ti.get("command")
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}
```
In `mod.rs`, add `pub command: Option<String>` to `PreBashCall` and `PostBashCall`.
In `claude.rs`, before building `context`, compute `let bash_command = parse::bash_command_from_tool_input(&data);` and set `command: bash_command.clone()` in the `PreBashCall`/`PostBashCall` constructions (cursor/gemini/codex analogously where they classify Bash; leave `None` where unavailable).
In `orchestrator.rs::execute_pre_bash_call`, compute `let start_ns = unix_nanos_now();` and thread `command` + `start_ns` through `bash_tool::handle_bash_pre_tool_use_with_context` → the `BashSessionStart` send. In `execute_post_bash_call`, compute `let end_ns = unix_nanos_now();` and thread into the `BashSessionEnd` send. Add a shared `fn unix_nanos_now() -> i64` (in `bash_tool.rs`, reusing `system_time_to_nanos` at ~795). Wherever `bash_tool` constructs `ControlRequest::BashSessionStart`/`End`, add the new fields.

NOTE: `PreBashCall`/`PostBashCall` carry `command` only so far as the preset supplies it; the orchestrator passes it down to the send site.

- [ ] **Step 4: Run test + build**

Run: `task test TEST_FILTER=test_bash_command_from_tool_input` then `task build`
Expected: PASS; compiles. Fix any `PreBashCall`/`PostBashCall` construction sites flagged by the compiler (tests in presets that build these literals must add `command: None`).

- [ ] **Step 5: Commit**

```bash
git add src/commands/checkpoint_agent/
git commit -m "feat(checkpoint): capture bash command + ns timestamps for tracking"
```

---

## Task 8: Extract shared `unknown_lines` helper

**Files:**
- Modify: `src/authorship/background_agent.rs:103-134` — extract.
- Create dependency for: `src/authorship/recovery/mod.rs` (Task 9).

**Interfaces:**
- Produces: `pub fn unknown_lines(authorship_log: &AuthorshipLog, committed_hunks: &HashMap<String, Vec<LineRange>>) -> HashMap<String, Vec<u32>>` — per-file sorted unknown line numbers (committed-but-unattributed). Living in a new shared location `src/authorship/recovery/mod.rs` is cleanest; `background_agent` then calls it.

- [ ] **Step 1: Write the failing test** — create `src/authorship/recovery/mod.rs` minimal with the helper + test, and add `pub mod recovery;` to `src/authorship/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::authorship_log_serialization::{AttestationEntry, AuthorshipLog};
    use crate::authorship::authorship_log::LineRange;
    use std::collections::HashMap;

    #[test]
    fn test_unknown_lines_subtracts_attributed() {
        let mut log = AuthorshipLog::new();
        log.get_or_create_file("a.rs")
            .add_entry(AttestationEntry::new("hash1".into(), vec![LineRange::Range(1, 3)]));
        let mut committed = HashMap::new();
        committed.insert("a.rs".to_string(), vec![LineRange::Range(1, 5)]);
        let unknown = unknown_lines(&log, &committed);
        assert_eq!(unknown.get("a.rs").unwrap(), &vec![4, 5]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_unknown_lines_subtracts_attributed`
Expected: FAIL — `recovery` module / `unknown_lines` missing.

- [ ] **Step 3: Implement** — in `recovery/mod.rs`:

```rust
use crate::authorship::authorship_log::LineRange;
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use std::collections::{HashMap, HashSet};

/// Per-file committed line numbers that have no attestation entry ("unknown").
pub fn unknown_lines(
    authorship_log: &AuthorshipLog,
    committed_hunks: &HashMap<String, Vec<LineRange>>,
) -> HashMap<String, Vec<u32>> {
    let mut attributed: HashMap<&str, HashSet<u32>> = HashMap::new();
    for fa in &authorship_log.attestations {
        let set = attributed.entry(&fa.file_path).or_default();
        for entry in &fa.entries {
            for range in &entry.line_ranges {
                for line in range.expand() {
                    set.insert(line);
                }
            }
        }
    }
    let mut out = HashMap::new();
    for (file, ranges) in committed_hunks {
        let existing = attributed.get(file.as_str());
        let mut unknown: Vec<u32> = Vec::new();
        for range in ranges {
            for line in range.expand() {
                if existing.is_none_or(|s| !s.contains(&line)) {
                    unknown.push(line);
                }
            }
        }
        if !unknown.is_empty() {
            unknown.sort();
            unknown.dedup();
            out.insert(file.clone(), unknown);
        }
    }
    out
}
```
Refactor `background_agent::fill_unattributed_lines` to call `crate::authorship::recovery::unknown_lines` instead of its inline loop, then `compress_lines` the result (keeps behavior identical).

- [ ] **Step 4: Run test + existing background_agent tests**

Run: `task test TEST_FILTER=unknown_lines` then `task test TEST_FILTER=background_agent`
Expected: PASS (no behavior change in background_agent).

- [ ] **Step 5: Commit**

```bash
git add src/authorship/recovery/mod.rs src/authorship/mod.rs src/authorship/background_agent.rs
git commit -m "refactor(authorship): extract shared unknown_lines helper"
```

---

## Task 9: Recovery orchestrator + solver trait + metric emission

**Files:**
- Modify: `src/authorship/recovery/mod.rs`

**Interfaces:**
- Consumes: `unknown_lines` (Task 8); `generate_session_id`, `generate_trace_id`, `AttestationEntry`, `SessionRecord`, `AgentId`, `LineRange::compress_lines`.
- Produces:
  - `pub struct RecoveryContext<'a> { repo, commit_sha, parent_sha, repo_work_dir: &Path, committed_hunks: &HashMap<String, Vec<LineRange>>, human_author: &str }`
  - `pub struct RecoveredCheckpointMetric { pub session_key: String, pub trace_id: String, pub agent_id: AgentId, pub file_path: String, pub lines_added: u32, pub edit_kind: String, pub checkpoint_ts: u64, pub recovery_metadata_json: String }`
  - `pub struct RecoveredAttribution { pub session_key: String, pub trace_id: String, pub session_record: SessionRecord, pub per_file_lines: HashMap<String, Vec<LineRange>>, pub metrics: Vec<RecoveredCheckpointMetric> }`
  - `pub struct AiLineOwner { pub session_key: String, pub agent_id: AgentId, pub edit_kind: String }`
  - `pub trait RecoverySolver { fn name(&self) -> &'static str; fn solve(&self, ctx: &RecoveryContext, unknown: &HashMap<String, Vec<u32>>, ai_lines: &HashMap<String, HashMap<u32, AiLineOwner>>) -> Vec<RecoveredAttribution>; }` — the 3-arg shape is defined here from the start; Solver 1 (Task 10) ignores `ai_lines`, Solver 2 (Task 11) uses it.
  - `fn ai_lines_from_log(log: &AuthorshipLog) -> HashMap<String, HashMap<u32, AiLineOwner>>` — classify each attestation: the session-key part (before `::` if present) is AI iff it is NOT prefixed `h_` and resolves in `metadata.sessions`/`metadata.prompts` to an AI `agent_id`.
  - `pub fn recover_attribution(log: &mut AuthorshipLog, ctx: &RecoveryContext, solvers: &[Box<dyn RecoverySolver>]) -> Vec<RecoveredCheckpointMetric>`
  - `pub fn apply_recovered(log: &mut AuthorshipLog, rec: &RecoveredAttribution)` — inserts SessionRecord + AttestationEntries (hash `format!("{session_key}::{trace_id}")`).

- [ ] **Step 1: Write the failing test** — add a stub solver test:

```rust
#[cfg(test)]
mod orch_tests {
    use super::*;
    use crate::authorship::authorship_log_serialization::AuthorshipLog;
    use crate::authorship::working_log::AgentId;
    use std::collections::HashMap;
    use std::path::Path;

    struct CoverAllSolver;
    impl RecoverySolver for CoverAllSolver {
        fn name(&self) -> &'static str { "cover_all" }
        fn solve(&self, _ctx: &RecoveryContext, unknown: &HashMap<String, Vec<u32>>,
                 _ai_lines: &HashMap<String, HashMap<u32, AiLineOwner>>) -> Vec<RecoveredAttribution> {
            if unknown.is_empty() { return vec![]; }
            let agent = AgentId { tool: "bash".into(), id: "x".into(), model: "m".into() };
            let session_key = "s_test".to_string();
            let mut per_file = HashMap::new();
            let mut metrics = vec![];
            for (f, lines) in unknown {
                per_file.insert(f.clone(), LineRange::compress_lines(lines));
                metrics.push(RecoveredCheckpointMetric {
                    session_key: session_key.clone(), trace_id: "t_test".into(),
                    agent_id: agent.clone(), file_path: f.clone(), lines_added: lines.len() as u32,
                    edit_kind: "bash".into(), checkpoint_ts: 1, recovery_metadata_json: "{}".into(),
                });
            }
            vec![RecoveredAttribution {
                session_key, trace_id: "t_test".into(),
                session_record: crate::authorship::authorship_log::SessionRecord {
                    agent_id: agent, human_author: None, custom_attributes: None },
                per_file_lines: per_file, metrics,
            }]
        }
    }

    #[test]
    fn test_recover_attribution_covers_unknown() {
        let mut log = AuthorshipLog::new();
        let mut committed = HashMap::new();
        committed.insert("a.rs".to_string(), vec![LineRange::Range(1, 2)]);
        let repo = crate::git::repository::Repository::open(".").unwrap();
        let ctx = RecoveryContext {
            repo: &repo, commit_sha: "c", parent_sha: "p",
            repo_work_dir: Path::new("/repo"), committed_hunks: &committed, human_author: "h",
        };
        let solvers: Vec<Box<dyn RecoverySolver>> = vec![Box::new(CoverAllSolver)];
        let metrics = recover_attribution(&mut log, &ctx, &solvers);
        assert_eq!(metrics.len(), 1);
        // line 1-2 of a.rs now attributed
        let fa = log.attestations.iter().find(|f| f.file_path == "a.rs").unwrap();
        assert!(!fa.entries.is_empty());
    }
}
```
(If `Repository::open(".")` is awkward in a unit test, gate the ctx.repo usage so the orchestrator does not deref repo — the orchestrator itself only passes ctx to solvers. Prefer making `recover_attribution` not touch `repo` directly.)

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_recover_attribution_covers_unknown`
Expected: FAIL — types/fn missing.

- [ ] **Step 3: Implement** — add structs, trait, and:

```rust
pub fn apply_recovered(log: &mut AuthorshipLog, rec: &RecoveredAttribution) {
    log.metadata.sessions.insert(rec.session_key.clone(), rec.session_record.clone());
    let hash = format!("{}::{}", rec.session_key, rec.trace_id);
    for (file, ranges) in &rec.per_file_lines {
        let fa = log.get_or_create_file(file);
        fa.add_entry(crate::authorship::authorship_log_serialization::AttestationEntry::new(
            hash.clone(), ranges.clone()));
    }
}

pub fn recover_attribution(
    log: &mut AuthorshipLog,
    ctx: &RecoveryContext,
    solvers: &[Box<dyn RecoverySolver>],
) -> Vec<RecoveredCheckpointMetric> {
    let mut all_metrics = Vec::new();
    for solver in solvers {
        let unknown = unknown_lines(log, ctx.committed_hunks);
        if unknown.is_empty() { break; }
        let ai_lines = ai_lines_from_log(log);
        let recovered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            solver.solve(ctx, &unknown, &ai_lines)
        })).unwrap_or_else(|_| {
            tracing::warn!("recovery solver {} panicked; skipping", solver.name());
            vec![]
        });
        for rec in recovered {
            apply_recovered(log, &rec);
            all_metrics.extend(rec.metrics);
        }
    }
    all_metrics
}
```
Note `LineRange::expand` exists (used by background_agent). Confirm `Repository` import path.

- [ ] **Step 4: Run test to verify it passes**

Run: `task test TEST_FILTER=test_recover_attribution_covers_unknown`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/authorship/recovery/mod.rs
git commit -m "feat(recovery): attribution-recovery orchestrator + solver trait"
```

---

## Task 10: Solver 1 — bash mtime/ctime correlation

**Files:**
- Create: `src/authorship/recovery/bash_solver.rs`
- Modify: `src/authorship/recovery/mod.rs` — `pub mod bash_solver;`

**Interfaces:**
- Consumes: `BashCheckpointsDatabase::{global, find_candidates}`; `StatEntry::from_metadata`; `RecoverySolver`, `RecoveredAttribution`, `RecoveredCheckpointMetric`, `RecoveryContext` (Task 9); `generate_session_id`, `generate_trace_id`.
- Produces: `pub struct BashCorrelationSolver { pub window_ns: i64 }` with `impl RecoverySolver`. Default window = `3 * 1_000_000_000`.

- [ ] **Step 1: Write the failing test** — use an env-overridden DB seeded with a row, and a temp worktree file whose mtime is set:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::recovery::{RecoveryContext, RecoverySolver};
    use crate::authorship::authorship_log::LineRange;
    use crate::daemon::bash_checkpoints_db::BashCheckpointsDatabase;
    use crate::authorship::working_log::AgentId;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[test]
    #[serial_test::serial(bash_ckpt_env)]
    fn test_bash_solver_matches_within_window() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("b.db");
        unsafe { std::env::set_var("GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH", &db_path); }

        // Write a file and read its real mtime in ns.
        let work = TempDir::new().unwrap();
        let file = work.path().join("out.txt");
        std::fs::write(&file, "generated\n").unwrap();
        let meta = std::fs::symlink_metadata(&file).unwrap();
        let mtime_ns = meta.modified().unwrap()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as i64;

        // Seed a bash checkpoint bracketing that mtime, keyed to the work dir.
        let repo_key = work.path().to_string_lossy().to_string();
        {
            let mut db = BashCheckpointsDatabase::open_at_path(&db_path).unwrap();
            let agent = AgentId { tool: "claude".into(), id: "sess".into(), model: "opus".into() };
            db.record_start("sess", "tu1", &repo_key, &agent, Some("touch out.txt"), mtime_ns - 1_000_000_000, 10).unwrap();
            db.record_end("sess", "tu1", mtime_ns + 1_000_000_000).unwrap();
        }

        let mut committed = HashMap::new();
        committed.insert("out.txt".to_string(), vec![LineRange::Single(1)]);
        let unknown: HashMap<String, Vec<u32>> =
            [("out.txt".to_string(), vec![1u32])].into_iter().collect();

        let solver = BashCorrelationSolver::default();
        // RecoveryContext.repo not used by this solver; pass a dummy via Option-free design.
        let ctx = RecoveryContext::for_test(work.path(), &committed, "h");
        let recovered = solver.solve(&ctx, &unknown, &HashMap::new());
        unsafe { std::env::remove_var("GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH"); }

        assert_eq!(recovered.len(), 1);
        assert!(recovered[0].per_file_lines.contains_key("out.txt"));
        assert_eq!(recovered[0].metrics[0].edit_kind, "bash");
    }

    #[test]
    #[serial_test::serial(bash_ckpt_env)]
    fn test_bash_solver_no_match_out_of_window() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("b.db");
        unsafe { std::env::set_var("GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH", &db_path); }
        let work = TempDir::new().unwrap();
        let file = work.path().join("out.txt");
        std::fs::write(&file, "x\n").unwrap();
        let repo_key = work.path().to_string_lossy().to_string();
        {
            let mut db = BashCheckpointsDatabase::open_at_path(&db_path).unwrap();
            let agent = AgentId { tool: "claude".into(), id: "s".into(), model: "m".into() };
            db.record_start("s", "t", &repo_key, &agent, None, 1, 10).unwrap();
            db.record_end("s", "t", 2).unwrap(); // ancient
        }
        let unknown: HashMap<String, Vec<u32>> = [("out.txt".to_string(), vec![1u32])].into_iter().collect();
        let committed = HashMap::new();
        let solver = BashCorrelationSolver::default();
        let ctx = RecoveryContext::for_test(work.path(), &committed, "h");
        let recovered = solver.solve(&ctx, &unknown, &HashMap::new());
        unsafe { std::env::remove_var("GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH"); }
        assert!(recovered.is_empty());
    }
}
```
Add a `RecoveryContext::for_test(repo_work_dir, committed_hunks, human_author)` test-only constructor to `mod.rs` that fills `repo`/`commit_sha`/`parent_sha` with placeholders the solver does not read. To support this cleanly, make `RecoveryContext.repo` an `Option<&Repository>` OR have the solver only read `repo_work_dir`/`committed_hunks` (preferred — Solver 1 needs only `repo_work_dir` + the worktree files). Define `for_test` under `#[cfg(test)]`.

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_bash_solver_matches_within_window`
Expected: FAIL — `BashCorrelationSolver` missing.

- [ ] **Step 3: Implement**

```rust
use crate::authorship::authorship_log::LineRange;
use crate::authorship::authorship_log_serialization::{generate_session_id, generate_trace_id};
use crate::authorship::recovery::{
    RecoveredAttribution, RecoveredCheckpointMetric, RecoveryContext, RecoverySolver,
};
use crate::authorship::authorship_log::SessionRecord;
use crate::authorship::working_log::AgentId;
use crate::daemon::bash_checkpoints_db::BashCheckpointsDatabase;
use std::collections::HashMap;

const DEFAULT_WINDOW_NS: i64 = 3 * 1_000_000_000;

pub struct BashCorrelationSolver {
    pub window_ns: i64,
}
impl Default for BashCorrelationSolver {
    fn default() -> Self { Self { window_ns: DEFAULT_WINDOW_NS } }
}

impl RecoverySolver for BashCorrelationSolver {
    fn name(&self) -> &'static str { "bash_correlation" }

    fn solve(&self, ctx: &RecoveryContext, unknown: &HashMap<String, Vec<u32>>,
             _ai_lines: &HashMap<String, HashMap<u32, crate::authorship::recovery::AiLineOwner>>) -> Vec<RecoveredAttribution> {
        let repo_key = ctx.repo_work_dir.to_string_lossy().to_string();
        let db_mutex = match BashCheckpointsDatabase::global() { Ok(d) => d, Err(_) => return vec![] };
        let mut out = Vec::new();
        for (file, lines) in unknown {
            let full = ctx.repo_work_dir.join(file);
            let meta = match std::fs::symlink_metadata(&full) { Ok(m) => m, Err(_) => continue };
            let mut file_times_ns: Vec<i64> = Vec::new();
            if let Ok(mt) = meta.modified() {
                file_times_ns.push(mt.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let c = meta.ctime() as i64 * 1_000_000_000 + meta.ctime_nsec() as i64;
                if c > 0 { file_times_ns.push(c); }
            }
            if file_times_ns.is_empty() { continue; }

            let lo = file_times_ns.iter().min().copied().unwrap() - self.window_ns;
            let hi = file_times_ns.iter().max().copied().unwrap() + self.window_ns;
            let candidates = {
                let guard = match db_mutex.lock() { Ok(g) => g, Err(_) => continue };
                guard.find_candidates(&repo_key, lo, hi).unwrap_or_default()
            };
            // Pick closest candidate to any file time (compare against end_ns, fallback start_ns).
            let mut best: Option<(i64, _)> = None;
            for cand in &candidates {
                let edge = cand.end_ns.unwrap_or(cand.start_ns);
                let delta = file_times_ns.iter().map(|t| (t - edge).abs()).min().unwrap();
                if delta <= self.window_ns && best.as_ref().is_none_or(|(d, _)| delta < *d) {
                    best = Some((delta, cand));
                }
            }
            let Some((delta, cand)) = best else { continue };

            let internal_id = cand.agent_internal_id.clone().unwrap_or_default();
            let session_key = generate_session_id(&internal_id, &cand.tool);
            let trace_id = generate_trace_id();
            let agent_id = AgentId {
                tool: cand.tool.clone(),
                id: internal_id,
                model: cand.agent_model.clone().unwrap_or_else(|| "unknown".to_string()),
            };
            let recovery_json = serde_json::json!({
                "solver": "bash_correlation",
                "tool_use_id": cand.tool_use_id,
                "command": cand.command.as_deref().map(|c| c.chars().take(256).collect::<String>()),
                "delta_ns": delta,
                "matched_edge": if cand.end_ns.is_some() { "end" } else { "start" },
            }).to_string();
            let ts = (cand.end_ns.unwrap_or(cand.start_ns) / 1_000_000_000).max(0) as u64;

            let mut per_file = HashMap::new();
            per_file.insert(file.clone(), LineRange::compress_lines(lines));
            out.push(RecoveredAttribution {
                session_key: session_key.clone(),
                trace_id: trace_id.clone(),
                session_record: SessionRecord { agent_id: agent_id.clone(), human_author: Some(ctx.human_author.to_string()), custom_attributes: None },
                per_file_lines: per_file,
                metrics: vec![RecoveredCheckpointMetric {
                    session_key, trace_id, agent_id, file_path: file.clone(),
                    lines_added: lines.len() as u32, edit_kind: "bash".into(),
                    checkpoint_ts: ts, recovery_metadata_json: recovery_json,
                }],
            });
        }
        out
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `task test TEST_FILTER=bash_solver`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add src/authorship/recovery/bash_solver.rs src/authorship/recovery/mod.rs
git commit -m "feat(recovery): Solver 1 — bash mtime/ctime correlation"
```

---

## Task 11: Solver 2 — AI edge extension

**Files:**
- Create: `src/authorship/recovery/edge_solver.rs`
- Modify: `src/authorship/recovery/mod.rs` — `pub mod edge_solver;`; add helper to classify an attestation hash as AI.

**Interfaces:**
- Consumes: `AuthorshipLog`, `metadata.sessions`/`prompts`/`humans`, `RecoverySolver`. Solver 2 needs the **log** (to see existing AI lines), which `unknown` alone doesn't carry — so its `solve` reads through a borrowed log. Add to the trait an optional richer entry: give `RecoveryContext` a field `ai_lines_by_file: HashMap<String, HashMap<u32, (String /*session_key*/, AgentId, String /*edit_kind*/)>>` populated by the orchestrator from the current log *before* each solver runs.
- Produces: `pub struct AiEdgeExtensionSolver;` with `impl RecoverySolver`.

- [ ] **Step 1: Write the failing test** — in `edge_solver.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::recovery::{RecoveryContext, RecoverySolver};
    use crate::authorship::working_log::AgentId;
    use std::collections::HashMap;
    use std::path::Path;

    fn owner(sk: &str) -> AiLineOwner {
        AiLineOwner { session_key: sk.into(),
            agent_id: AgentId { tool: "claude".into(), id: "x".into(), model: "m".into() },
            edit_kind: "file_edit".into() }
    }

    #[test]
    fn test_edge_extension_absorbs_adjacent_below() {
        // AI owns lines 1-3; line 4 is unknown and directly below → absorbed.
        let mut ai_map: HashMap<u32, AiLineOwner> = HashMap::new();
        for l in 1..=3 { ai_map.insert(l, owner("s_ai")); }
        let ai_lines: HashMap<String, HashMap<u32, AiLineOwner>> =
            [("a.rs".to_string(), ai_map)].into_iter().collect();
        let committed = HashMap::new();
        let ctx = RecoveryContext::for_test(Path::new("/r"), &committed, "h");
        let unknown: HashMap<String, Vec<u32>> = [("a.rs".to_string(), vec![4u32])].into_iter().collect();

        let recovered = AiEdgeExtensionSolver.solve(&ctx, &unknown, &ai_lines);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].session_key, "s_ai");
        assert!(recovered[0].per_file_lines["a.rs"].iter().any(|r| r.expand().contains(&4)));
    }

    #[test]
    fn test_edge_extension_skips_non_adjacent() {
        let ai_map: HashMap<u32, AiLineOwner> = [(1u32, owner("s_ai"))].into_iter().collect();
        let ai_lines: HashMap<String, HashMap<u32, AiLineOwner>> =
            [("a.rs".to_string(), ai_map)].into_iter().collect();
        let committed = HashMap::new();
        let ctx = RecoveryContext::for_test(Path::new("/r"), &committed, "h");
        // unknown line 10 is far from AI line 1 → untouched
        let unknown: HashMap<String, Vec<u32>> = [("a.rs".to_string(), vec![10u32])].into_iter().collect();
        assert!(AiEdgeExtensionSolver.solve(&ctx, &unknown, &ai_lines).is_empty());
    }

    #[test]
    fn test_edge_extension_no_steal_from_human_only() {
        // No AI lines at all for the file → nothing absorbed.
        let ai_lines: HashMap<String, HashMap<u32, AiLineOwner>> = HashMap::new();
        let committed = HashMap::new();
        let ctx = RecoveryContext::for_test(Path::new("/r"), &committed, "h");
        let unknown: HashMap<String, Vec<u32>> = [("a.rs".to_string(), vec![2u32])].into_iter().collect();
        assert!(AiEdgeExtensionSolver.solve(&ctx, &unknown, &ai_lines).is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_edge_extension_absorbs_adjacent_below`
Expected: FAIL — solver + `ctx.ai_lines_by_file` missing.

- [ ] **Step 3: Implement**

The trait already takes `ai_lines: &HashMap<String, HashMap<u32, AiLineOwner>>` (defined in Task 9), and the orchestrator already computes it per-iteration via `ai_lines_from_log` and passes it to every solver. So Task 11 only implements `AiEdgeExtensionSolver` against that existing param — no trait churn. The `RecoveryContext::for_test` helper sets `ai_lines` via a separate arg in these unit tests by constructing the map directly (the orchestrator path computes it from the log at runtime).

NOTE on the unit tests above: they call `solver.solve(&ctx, &unknown, &ai_lines)` — adjust the test bodies to build the `ai_lines` map and pass it as the third arg rather than stashing it on `ctx`. Keep `RecoveryContext::for_test` minimal (repo_work_dir + committed_hunks + human_author).

`edge_solver.rs`:
```rust
impl RecoverySolver for AiEdgeExtensionSolver {
    fn name(&self) -> &'static str { "ai_edge_extension" }
    fn solve(&self, _ctx: &RecoveryContext, unknown: &HashMap<String, Vec<u32>>,
             ai_lines: &HashMap<String, HashMap<u32, AiLineOwner>>) -> Vec<RecoveredAttribution> {
        let mut out = Vec::new();
        for (file, lines) in unknown {
            let Some(ai_map) = ai_lines.get(file) else { continue };
            if ai_map.is_empty() { continue; }
            // group unknown lines into maximal contiguous runs
            for run in contiguous_runs(lines) {
                let above = run.first().copied().unwrap().saturating_sub(1);
                let below = run.last().copied().unwrap() + 1;
                let owner = ai_map.get(&above).or_else(|| ai_map.get(&below));
                let Some(owner) = owner else { continue };
                let trace_id = generate_trace_id();
                let mut per_file = HashMap::new();
                per_file.insert(file.clone(), LineRange::compress_lines(&run));
                out.push(RecoveredAttribution {
                    session_key: owner.session_key.clone(),
                    trace_id: trace_id.clone(),
                    session_record: SessionRecord { agent_id: owner.agent_id.clone(), human_author: None, custom_attributes: None },
                    per_file_lines: per_file,
                    metrics: vec![RecoveredCheckpointMetric {
                        session_key: owner.session_key.clone(), trace_id,
                        agent_id: owner.agent_id.clone(), file_path: file.clone(),
                        lines_added: run.len() as u32, edit_kind: owner.edit_kind.clone(),
                        checkpoint_ts: 0,
                        recovery_metadata_json: serde_json::json!({
                            "solver": "ai_edge_extension",
                            "extended_from_session": owner.session_key,
                            "adjacent_side": if ai_map.contains_key(&above) {"above"} else {"below"},
                            "run_lines": run.len(),
                        }).to_string(),
                    }],
                });
            }
        }
        out
    }
}

fn contiguous_runs(sorted: &[u32]) -> Vec<Vec<u32>> {
    let mut runs = Vec::new();
    let mut cur: Vec<u32> = Vec::new();
    for &l in sorted {
        if cur.last().is_some_and(|&p| l != p + 1) { runs.push(std::mem::take(&mut cur)); }
        cur.push(l);
    }
    if !cur.is_empty() { runs.push(cur); }
    runs
}
```
Update the trait + Task 9 orchestrator + Task 10 solver signature to the 3-arg `solve`.

- [ ] **Step 4: Run test to verify it passes**

Run: `task test TEST_FILTER=edge_solver` then `task test TEST_FILTER=recovery`
Expected: PASS. Re-run `bash_solver` to confirm signature change didn't break it.

- [ ] **Step 5: Commit**

```bash
git add src/authorship/recovery/edge_solver.rs src/authorship/recovery/mod.rs src/authorship/recovery/bash_solver.rs
git commit -m "feat(recovery): Solver 2 — AI edge extension"
```

---

## Task 12: Wire recovery into post-commit + emit metrics

**Files:**
- Modify: `src/authorship/post_commit.rs:205-256`

**Interfaces:**
- Consumes: `recover_attribution`, `BashCorrelationSolver`, `AiEdgeExtensionSolver`, `RecoveryContext`, `RecoveredCheckpointMetric`.
- Produces: recovery runs after `fill_unattributed_lines`, before `transform`. Adds `emit_recovered_metrics(...)`.

- [ ] **Step 1: Write the failing test** — integration test in `tests/integration/recovery.rs` (new), registered in the integration mod. Use the custom-checkpoint flow + a seeded bash row. Add to `tests/integration/mod.rs`: `mod recovery;`.

```rust
use crate::repos::test_repo::TestRepo;
use crate::repos::test_file::lines;

#[test]
fn test_bash_recovery_attributes_untracked_file() {
    let repo = TestRepo::new();
    // (helper seeds a bash checkpoint row in the repo's bash-checkpoints DB bracketing now)
    let path = repo.path().join("script_out.txt");
    std::fs::write(&path, "line one\nline two\n").unwrap();
    repo.seed_bash_checkpoint("script_out.txt", "echo hi"); // helper (Step 3)
    repo.git(&["add", "."]).unwrap();
    repo.commit("add generated file").unwrap();
    let mut file = repo.filename("script_out.txt");
    file.assert_committed_lines(lines!["line one".ai(), "line two".ai()]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_bash_recovery_attributes_untracked_file`
Expected: FAIL — lines untracked (recovery not wired) / helper missing.

- [ ] **Step 3: Implement**

Hoist `committed_hunks` computation above the no-hooks branch so it's available unconditionally (compute once; reuse for both `fill_unattributed_lines` and recovery). After the `fill_unattributed_lines` block and before `transform`:
```rust
    if Config::fresh().get_feature_flags().attribution_recovery
        && let Some(ref committed_hunks) = committed_hunks_opt
    {
        let repo_work_dir = repo.workdir().unwrap_or_default();
        let ctx = crate::authorship::recovery::RecoveryContext {
            repo, commit_sha: &commit_sha, parent_sha: &parent_sha,
            repo_work_dir: &repo_work_dir, committed_hunks, human_author: &human_author,
        };
        let solvers: Vec<Box<dyn crate::authorship::recovery::RecoverySolver>> = vec![
            Box::new(crate::authorship::recovery::bash_solver::BashCorrelationSolver::default()),
            Box::new(crate::authorship::recovery::edge_solver::AiEdgeExtensionSolver),
        ];
        let metrics = crate::authorship::recovery::recover_attribution(&mut authorship_log, &ctx, &solvers);
        crate::authorship::recovery::emit_recovered_metrics(repo, &commit_sha, &parent_sha, &metrics);
    }
```
Feature-flag accessor is `Config::fresh().get_feature_flags().<flag>` (honors `GIT_AI_TEST_CONFIG_PATCH`/`set_test_feature_flags`). Implement `emit_recovered_metrics` in `recovery/mod.rs`:
```rust
pub fn emit_recovered_metrics(
    repo: &crate::git::repository::Repository,
    commit_sha: &str,
    parent_sha: &str,
    metrics: &[RecoveredCheckpointMetric],
) {
    for m in metrics {
        let values = crate::metrics::CheckpointValues::new()
            .checkpoint_ts(m.checkpoint_ts)
            .kind("ai_agent")
            .file_path(m.file_path.clone())
            .lines_added(m.lines_added)
            .edit_kind(m.edit_kind.clone())
            .attribution_recovery_metadata(m.recovery_metadata_json.clone());
        let attrs = crate::metrics::EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
            .session_id(m.session_key.clone())
            .trace_id(m.trace_id.clone())
            .tool(m.agent_id.tool.clone())
            .model(m.agent_id.model.clone());
        let _ = (repo, commit_sha, parent_sha); // repo/commit attrs added via build_checkpoint_attrs if desired
        crate::metrics::record(values, attrs);
    }
}
```
(If `EventAttributes` builder methods differ, mirror `daemon/checkpoint.rs:343-382`.)

Add the `TestRepo::seed_bash_checkpoint` helper in `tests/integration/repos/test_repo.rs`: it opens `BashCheckpointsDatabase::open_at_path(repo.bash_ckpt_db_path())` and inserts a row keyed to the repo workdir with start/end bracketing `SystemTime::now()` in ns, and ensures the subprocess env sets `GIT_AI_TEST_BASH_CHECKPOINTS_DB_PATH` to that path (so the daemon reads the same DB). Mirror the existing `GIT_AI_TEST_NOTES_DB_PATH` wiring in `test_repo.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `task test TEST_FILTER=test_bash_recovery_attributes_untracked_file`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/authorship/post_commit.rs src/authorship/recovery/mod.rs tests/integration/
git commit -m "feat(post-commit): run attribution recovery + emit recovered metrics"
```

---

## Task 13: Integration coverage — timing miss, edge extension, no-steal, ordering

**Files:**
- Modify: `tests/integration/recovery.rs`

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn test_bash_recovery_timing_miss_stays_untracked() {
    let repo = TestRepo::new();
    let path = repo.path().join("orphan.txt");
    std::fs::write(&path, "no owner\n").unwrap();
    repo.seed_bash_checkpoint_at("orphan.txt", "echo hi", /*ns_offset_secs*/ -3600); // far in past
    repo.git(&["add", "."]).unwrap();
    repo.commit("c").unwrap();
    repo.filename("orphan.txt").assert_committed_lines(lines!["no owner".unattributed_human()]);
}

#[test]
fn test_edge_extension_absorbs_trailing_untracked_line() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("m.txt");
    // AI writes lines 1-2; an untracked blank/extra line 3 lands adjacent.
    std::fs::write(&file_path, "ai one\nai two\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "m.txt"]).unwrap();
    std::fs::write(&file_path, "ai one\nai two\nuntracked edge\n").unwrap();
    // no checkpoint for the third line (untracked)
    repo.stage_all_and_commit("c").unwrap();
    repo.filename("m.txt").assert_committed_lines(lines![
        "ai one".ai(), "ai two".ai(), "untracked edge".ai(), // extended
    ]);
}

#[test]
fn test_edge_extension_does_not_steal_human_adjacent() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("h.txt");
    std::fs::write(&file_path, "human one\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "h.txt"]).unwrap();
    std::fs::write(&file_path, "human one\nuntracked\n").unwrap();
    repo.stage_all_and_commit("c").unwrap();
    repo.filename("h.txt").assert_committed_lines(lines![
        "human one".human(), "untracked".unattributed_human(), // NOT extended
    ]);
}
```
Add `seed_bash_checkpoint_at` (offset variant) to `test_repo.rs`.

- [ ] **Step 2: Run to verify they fail (where expected)**

Run: `task test TEST_FILTER=recovery`
Expected: the edge-extension test may already pass after Task 12; timing-miss + no-steal should pass; fix any that fail.

- [ ] **Step 3: Implement** — only test-helper additions if a test reveals a gap.

- [ ] **Step 4: Run to verify all pass**

Run: `task test TEST_FILTER=recovery`
Expected: PASS (all recovery tests).

- [ ] **Step 5: Commit**

```bash
git add tests/integration/recovery.rs tests/integration/repos/test_repo.rs
git commit -m "test(recovery): timing-miss, edge-extension, no-steal, ordering"
```

---

## Task 14: Update existing tests broken by edge extension

**Files:**
- Modify: assorted `tests/integration/*.rs` and `src/**` snapshot/assertion sites where untracked lines sit directly adjacent to AI lines.

**Interfaces:** none new.

- [ ] **Step 1: Run the full suite to surface fallout**

Run: `task test`
Expected: some failures where a previously-`unattributed_human()` line adjacent to `.ai()` is now `.ai()`.

- [ ] **Step 2: Triage** — for each failure, confirm the new attribution is *correct* per the edge-extension rule (untracked run directly adjacent to AI, not human). If correct, update the assertion to `.ai()`. If the line is adjacent to human or isolated, it's a real bug — fix the solver, not the test.

- [ ] **Step 3: Update assertions / snapshots**

For `insta` snapshots: `cargo insta review` and accept only the legitimate edge-extension changes. For `lines!`/`assert_committed_lines`: change `.unattributed_human()` → `.ai()` at confirmed edges.

- [ ] **Step 4: Re-run full suite**

Run: `task test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "test: update edge-adjacent attribution expectations for AI edge extension"
```

---

## Task 15: Lint, format, final review

**Files:** whole branch.

- [ ] **Step 1:** Run `task fmt`
- [ ] **Step 2:** Run `task lint` — resolve all clippy warnings (no `#[allow]` band-aids unless matching existing patterns).
- [ ] **Step 3:** Run `task test` (full) — green.
- [ ] **Step 4:** Self-review the diff for: DRY (no duplicated unknown-line logic), error handling (all DB calls best-effort), feature-flag gating, no spawn-per-commit/file regressions in the recovery path (Solver 1 does N `lstat`s + one DB query per unknown file — acceptable, no git spawns).
- [ ] **Step 5: Commit** any fixes:

```bash
git add -A
git commit -m "chore: lint + fmt for bash attribution recovery"
```

---

## Self-Review (plan vs spec)

**Spec coverage:**
- Component 1 (DB) → Tasks 3-5. ✓
- Component 2 (control plumbing) → Tasks 6-7. ✓
- Component 3 (recovery pipeline: orchestrator, Solver 1, Solver 2) → Tasks 8-11. ✓
- Component 4 (recovered metric events + new attribute) → Tasks 1, 12. ✓
- 30-day retention → Task 5. ✓
- ±3s window + closest match + cover whole file → Task 10. ✓
- `recovered_bash` metric (kind ai_agent, edit_kind bash, ts/session of original) → Tasks 10, 12. ✓
- Edge extension carries session id + new trace id + metric → Task 11. ✓
- Existing-test fallout from edge solver → Task 14. ✓
- Feature flags → Task 2. ✓
- DRY `unknown_lines` shared with background_agent → Task 8. ✓

**Open risk flagged for executor:** the trait `solve` signature gains a 3rd arg (`ai_lines: &HashMap<String, HashMap<u32, AiLineOwner>>`) in Task 11; define the trait with all three args from the start in Task 9 (Solver 1 ignores `ai_lines`) to avoid a mid-plan signature change. Feature-flag accessor is standardized as `Config::fresh().get_feature_flags().<flag>` throughout.
