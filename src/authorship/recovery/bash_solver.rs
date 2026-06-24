//! Solver 1: recover attribution by correlating an untracked file's
//! modification/change time with a recorded bash checkpoint.
//!
//! For each file with unknown lines we read its mtime and ctime, query the
//! bash-checkpoints DB for tool-calls in the same repo whose `[start, end]`
//! interval is within ±`window_ns` of either timestamp, pick the closest match,
//! and cover the file's unknown lines to that bash session.

use crate::authorship::authorship_log::{LineRange, SessionRecord};
use crate::authorship::authorship_log_serialization::{generate_session_id, generate_trace_id};
use crate::authorship::recovery::{
    AiLineOwner, RecoveredAttribution, RecoveredCheckpointMetric, RecoveryContext, RecoverySolver,
};
use crate::authorship::working_log::AgentId;
use crate::daemon::bash_checkpoints_db::{BashCheckpointRow, BashCheckpointsDatabase};
use std::collections::HashMap;
use std::path::PathBuf;

/// Default correlation window: ±3 seconds.
const DEFAULT_WINDOW_NS: i64 = 3 * 1_000_000_000;

pub struct BashCorrelationSolver {
    pub window_ns: i64,
    /// Explicit DB path for deterministic tests. `None` ⇒ process-global DB
    /// (the production path; avoids the OnceLock-in-tests aliasing problem).
    db_path: Option<PathBuf>,
}

impl Default for BashCorrelationSolver {
    fn default() -> Self {
        Self {
            window_ns: DEFAULT_WINDOW_NS,
            db_path: None,
        }
    }
}

impl BashCorrelationSolver {
    #[cfg(test)]
    fn with_db_path(path: PathBuf) -> Self {
        Self {
            window_ns: DEFAULT_WINDOW_NS,
            db_path: Some(path),
        }
    }

    /// Query candidate bash checkpoints, routing through an explicit DB path
    /// (tests) or the process-global DB (production).
    fn query_candidates(
        &self,
        repo_key: &str,
        lo: i64,
        hi: i64,
    ) -> Vec<BashCheckpointRow> {
        if let Some(path) = &self.db_path {
            return BashCheckpointsDatabase::open_at_path(path)
                .ok()
                .and_then(|db| db.find_candidates(repo_key, lo, hi).ok())
                .unwrap_or_default();
        }
        let Ok(db_mutex) = BashCheckpointsDatabase::global() else {
            return vec![];
        };
        let Ok(guard) = db_mutex.lock() else {
            return vec![];
        };
        guard.find_candidates(repo_key, lo, hi).unwrap_or_default()
    }
}

impl RecoverySolver for BashCorrelationSolver {
    fn name(&self) -> &'static str {
        "bash_correlation"
    }

    fn solve(
        &self,
        ctx: &RecoveryContext,
        unknown: &HashMap<String, Vec<u32>>,
        _ai_lines: &HashMap<String, HashMap<u32, AiLineOwner>>,
    ) -> Vec<RecoveredAttribution> {
        // Match the daemon's `worktree_state_key`: canonicalize the worktree
        // path so the stored `repo_work_dir` and our query key agree.
        let repo_key = std::fs::canonicalize(ctx.repo_work_dir)
            .unwrap_or_else(|_| ctx.repo_work_dir.to_path_buf())
            .to_string_lossy()
            .to_string();
        let mut out = Vec::new();
        for (file, lines) in unknown {
            let full = ctx.repo_work_dir.join(file);
            let Ok(meta) = std::fs::symlink_metadata(&full) else {
                continue;
            };

            let mut file_times_ns: Vec<i64> = Vec::new();
            if let Ok(mt) = meta.modified() {
                file_times_ns.push(system_time_to_ns(mt));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let c = meta.ctime().saturating_mul(1_000_000_000) + meta.ctime_nsec();
                if c > 0 {
                    file_times_ns.push(c);
                }
            }
            file_times_ns.retain(|&t| t > 0);
            if file_times_ns.is_empty() {
                continue;
            }

            let lo = file_times_ns.iter().min().copied().unwrap() - self.window_ns;
            let hi = file_times_ns.iter().max().copied().unwrap() + self.window_ns;
            let candidates = self.query_candidates(&repo_key, lo, hi);

            // Pick the candidate whose edge (end, else start) is closest to any
            // of the file's timestamps, within the window.
            let mut best: Option<(i64, crate::daemon::bash_checkpoints_db::BashCheckpointRow)> =
                None;
            for cand in candidates {
                let edge = cand.end_ns.unwrap_or(cand.start_ns);
                let delta = file_times_ns
                    .iter()
                    .map(|t| (t - edge).abs())
                    .min()
                    .unwrap();
                if delta <= self.window_ns && best.as_ref().is_none_or(|(d, _)| delta < *d) {
                    best = Some((delta, cand));
                }
            }
            let Some((delta, cand)) = best else {
                continue;
            };

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
                "command": cand.command.as_deref().map(truncate_command),
                "delta_ns": delta,
                "matched_edge": if cand.end_ns.is_some() { "end" } else { "start" },
            })
            .to_string();
            let ts = (cand.end_ns.unwrap_or(cand.start_ns) / 1_000_000_000).max(0) as u64;

            let mut per_file = HashMap::new();
            per_file.insert(file.clone(), LineRange::compress_lines(lines));
            out.push(RecoveredAttribution {
                session_key: session_key.clone(),
                trace_id: trace_id.clone(),
                session_record: SessionRecord {
                    agent_id: agent_id.clone(),
                    human_author: Some(ctx.human_author.to_string()),
                    custom_attributes: None,
                },
                per_file_lines: per_file,
                metrics: vec![RecoveredCheckpointMetric {
                    session_key,
                    trace_id,
                    agent_id,
                    file_path: file.clone(),
                    lines_added: lines.len() as u32,
                    edit_kind: "bash".into(),
                    checkpoint_ts: ts,
                    recovery_metadata_json: recovery_json,
                }],
            });
        }
        out
    }
}

fn system_time_to_ns(t: std::time::SystemTime) -> i64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn truncate_command(c: &str) -> String {
    c.chars().take(256).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::authorship_log::LineRange;
    use crate::authorship::recovery::RecoveryContext;
    use crate::authorship::working_log::AgentId;
    use crate::daemon::bash_checkpoints_db::BashCheckpointsDatabase;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn agent() -> AgentId {
        AgentId {
            tool: "claude".into(),
            id: "sess".into(),
            model: "opus".into(),
        }
    }

    #[test]
    fn test_bash_solver_matches_within_window() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("b.db");

        let work = TempDir::new().unwrap();
        let file = work.path().join("out.txt");
        std::fs::write(&file, "generated\n").unwrap();
        let meta = std::fs::symlink_metadata(&file).unwrap();
        let mtime_ns = system_time_to_ns(meta.modified().unwrap());

        let repo_key = std::fs::canonicalize(work.path()).unwrap().to_string_lossy().to_string();
        {
            let mut db = BashCheckpointsDatabase::open_at_path(&db_path).unwrap();
            db.record_start(
                "sess",
                "tu1",
                &repo_key,
                &agent(),
                Some("touch out.txt"),
                mtime_ns - 1_000_000_000,
                10,
            )
            .unwrap();
            db.record_end("sess", "tu1", mtime_ns + 1_000_000_000)
                .unwrap();
        }

        let mut committed = HashMap::new();
        committed.insert("out.txt".to_string(), vec![LineRange::Single(1)]);
        let unknown: HashMap<String, Vec<u32>> =
            [("out.txt".to_string(), vec![1u32])].into_iter().collect();

        let solver = BashCorrelationSolver::with_db_path(db_path);
        let ctx = RecoveryContext::for_test(work.path(), &committed, "h");
        let recovered = solver.solve(&ctx, &unknown, &HashMap::new());

        assert_eq!(recovered.len(), 1);
        assert!(recovered[0].per_file_lines.contains_key("out.txt"));
        assert_eq!(recovered[0].metrics[0].edit_kind, "bash");
    }

    #[test]
    fn test_bash_solver_no_match_out_of_window() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("b.db");
        let work = TempDir::new().unwrap();
        let file = work.path().join("out.txt");
        std::fs::write(&file, "x\n").unwrap();
        let repo_key = std::fs::canonicalize(work.path()).unwrap().to_string_lossy().to_string();
        {
            let mut db = BashCheckpointsDatabase::open_at_path(&db_path).unwrap();
            db.record_start("s", "t", &repo_key, &agent(), None, 1, 10)
                .unwrap();
            db.record_end("s", "t", 2).unwrap();
        }
        let unknown: HashMap<String, Vec<u32>> =
            [("out.txt".to_string(), vec![1u32])].into_iter().collect();
        let committed = HashMap::new();
        let solver = BashCorrelationSolver::with_db_path(db_path);
        let ctx = RecoveryContext::for_test(work.path(), &committed, "h");
        let recovered = solver.solve(&ctx, &unknown, &HashMap::new());
        assert!(recovered.is_empty());
    }

    #[test]
    fn test_bash_solver_ignores_other_repo() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("b.db");
        let work = TempDir::new().unwrap();
        let file = work.path().join("out.txt");
        std::fs::write(&file, "x\n").unwrap();
        let meta = std::fs::symlink_metadata(&file).unwrap();
        let mtime_ns = system_time_to_ns(meta.modified().unwrap());
        {
            let mut db = BashCheckpointsDatabase::open_at_path(&db_path).unwrap();
            // Candidate bracketing the mtime but for a DIFFERENT repo.
            db.record_start(
                "s",
                "t",
                "/some/other/repo",
                &agent(),
                None,
                mtime_ns - 1_000_000_000,
                10,
            )
            .unwrap();
            db.record_end("s", "t", mtime_ns + 1_000_000_000).unwrap();
        }
        let unknown: HashMap<String, Vec<u32>> =
            [("out.txt".to_string(), vec![1u32])].into_iter().collect();
        let committed = HashMap::new();
        let solver = BashCorrelationSolver::with_db_path(db_path);
        let ctx = RecoveryContext::for_test(work.path(), &committed, "h");
        let recovered = solver.solve(&ctx, &unknown, &HashMap::new());
        assert!(recovered.is_empty());
    }
}
