use std::path::{Path, PathBuf};

use super::sqlite_ffi::Database;
use super::telemetry_types::{CasObject, MetricEvent};

const MAX_ROWS: usize = 50_000;
const MAX_DB_SIZE_BYTES: u64 = 10 * 1024 * 1024;

pub struct TelemetryQueue {
    db: Database,
    db_path: PathBuf,
}

impl TelemetryQueue {
    pub fn open(db_path: &Path) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create telemetry queue dir: {}", e))?;
        }

        let db = Database::open(db_path)?;

        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS pending_metrics (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 payload TEXT NOT NULL,
                 created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
             );
             CREATE TABLE IF NOT EXISTS pending_cas (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 payload TEXT NOT NULL,
                 created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
             );",
        )?;

        Ok(Self {
            db,
            db_path: db_path.to_path_buf(),
        })
    }

    pub fn enqueue_metrics(&self, events: &[MetricEvent]) -> Result<(), String> {
        if events.is_empty() {
            return Ok(());
        }
        self.enforce_limits("pending_metrics")?;
        let payload =
            serde_json::to_string(events).map_err(|e| format!("serialize metrics: {}", e))?;
        let stmt = self
            .db
            .prepare("INSERT INTO pending_metrics (payload) VALUES (?1)")?;
        stmt.bind_text(1, &payload)?;
        stmt.step()?;
        Ok(())
    }

    pub fn enqueue_cas(&self, objects: &[CasObject]) -> Result<(), String> {
        if objects.is_empty() {
            return Ok(());
        }
        self.enforce_limits("pending_cas")?;
        let payload =
            serde_json::to_string(objects).map_err(|e| format!("serialize cas: {}", e))?;
        let stmt = self
            .db
            .prepare("INSERT INTO pending_cas (payload) VALUES (?1)")?;
        stmt.bind_text(1, &payload)?;
        stmt.step()?;
        Ok(())
    }

    pub fn drain_metrics(&self, limit: usize) -> Result<Vec<(i64, Vec<MetricEvent>)>, String> {
        let stmt = self
            .db
            .prepare("SELECT id, payload FROM pending_metrics ORDER BY id ASC LIMIT ?1")?;
        stmt.bind_i64(1, limit as i64)?;

        let mut result = Vec::new();
        while stmt.step()? {
            let id = stmt.column_i64(0);
            let payload = stmt.column_text(1);
            let events: Vec<MetricEvent> = serde_json::from_str(&payload)
                .map_err(|e| format!("deserialize metrics: {}", e))?;
            result.push((id, events));
        }
        Ok(result)
    }

    pub fn drain_cas(&self, limit: usize) -> Result<Vec<(i64, Vec<CasObject>)>, String> {
        let stmt = self
            .db
            .prepare("SELECT id, payload FROM pending_cas ORDER BY id ASC LIMIT ?1")?;
        stmt.bind_i64(1, limit as i64)?;

        let mut result = Vec::new();
        while stmt.step()? {
            let id = stmt.column_i64(0);
            let payload = stmt.column_text(1);
            let objects: Vec<CasObject> =
                serde_json::from_str(&payload).map_err(|e| format!("deserialize cas: {}", e))?;
            result.push((id, objects));
        }
        Ok(result)
    }

    pub fn delete_metrics(&self, ids: &[i64]) -> Result<(), String> {
        for id in ids {
            let stmt = self
                .db
                .prepare("DELETE FROM pending_metrics WHERE id = ?1")?;
            stmt.bind_i64(1, *id)?;
            stmt.step()?;
        }
        Ok(())
    }

    pub fn delete_cas(&self, ids: &[i64]) -> Result<(), String> {
        for id in ids {
            let stmt = self.db.prepare("DELETE FROM pending_cas WHERE id = ?1")?;
            stmt.bind_i64(1, *id)?;
            stmt.step()?;
        }
        Ok(())
    }

    pub fn pending_metrics_count(&self) -> Result<usize, String> {
        self.row_count("pending_metrics")
    }

    pub fn pending_cas_count(&self) -> Result<usize, String> {
        self.row_count("pending_cas")
    }

    fn row_count(&self, table: &str) -> Result<usize, String> {
        let sql = format!("SELECT COUNT(*) FROM {}", table);
        let stmt = self.db.prepare(&sql)?;
        if stmt.step()? {
            Ok(stmt.column_i64(0) as usize)
        } else {
            Ok(0)
        }
    }

    fn enforce_limits(&self, table: &str) -> Result<(), String> {
        let total = self.row_count("pending_metrics")? + self.row_count("pending_cas")?;
        if total >= MAX_ROWS {
            self.evict_oldest(table)?;
        }

        if let Ok(meta) = std::fs::metadata(&self.db_path)
            && meta.len() >= MAX_DB_SIZE_BYTES
        {
            self.evict_oldest(table)?;
        }
        Ok(())
    }

    fn evict_oldest(&self, table: &str) -> Result<(), String> {
        let sql = format!(
            "DELETE FROM {} WHERE id IN (SELECT id FROM {} ORDER BY id ASC LIMIT 100)",
            table, table
        );
        self.db.execute_batch(&sql)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::telemetry_types::{MetricEventId, SparseArray};

    fn make_event() -> MetricEvent {
        MetricEvent::new(
            MetricEventId::Committed,
            SparseArray::new(),
            SparseArray::new(),
        )
    }

    fn make_cas() -> CasObject {
        CasObject {
            content: serde_json::json!({"test": true}),
            hash: "deadbeef".repeat(8),
            metadata: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn enqueue_and_drain_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let db = TelemetryQueue::open(&dir.path().join("tq.db")).unwrap();

        db.enqueue_metrics(&[make_event(), make_event()]).unwrap();
        assert_eq!(db.pending_metrics_count().unwrap(), 1);

        let batches = db.drain_metrics(10).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].1.len(), 2);
    }

    #[test]
    fn enqueue_and_drain_cas() {
        let dir = tempfile::tempdir().unwrap();
        let db = TelemetryQueue::open(&dir.path().join("tq.db")).unwrap();

        db.enqueue_cas(&[make_cas()]).unwrap();
        assert_eq!(db.pending_cas_count().unwrap(), 1);

        let batches = db.drain_cas(10).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].1.len(), 1);
    }

    #[test]
    fn delete_after_drain() {
        let dir = tempfile::tempdir().unwrap();
        let db = TelemetryQueue::open(&dir.path().join("tq.db")).unwrap();

        db.enqueue_metrics(&[make_event()]).unwrap();
        db.enqueue_metrics(&[make_event()]).unwrap();
        assert_eq!(db.pending_metrics_count().unwrap(), 2);

        let batches = db.drain_metrics(10).unwrap();
        let ids: Vec<i64> = batches.iter().map(|(id, _)| *id).collect();
        db.delete_metrics(&ids).unwrap();
        assert_eq!(db.pending_metrics_count().unwrap(), 0);
    }

    #[test]
    fn empty_enqueue_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let db = TelemetryQueue::open(&dir.path().join("tq.db")).unwrap();
        db.enqueue_metrics(&[]).unwrap();
        db.enqueue_cas(&[]).unwrap();
        assert_eq!(db.pending_metrics_count().unwrap(), 0);
        assert_eq!(db.pending_cas_count().unwrap(), 0);
    }

    #[test]
    fn persistence_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("persist.db");

        {
            let db = TelemetryQueue::open(&path).unwrap();
            db.enqueue_metrics(&[make_event()]).unwrap();
            db.enqueue_cas(&[make_cas(), make_cas()]).unwrap();
        }

        let db = TelemetryQueue::open(&path).unwrap();
        assert_eq!(db.pending_metrics_count().unwrap(), 1);
        assert_eq!(db.pending_cas_count().unwrap(), 1);
    }
}
