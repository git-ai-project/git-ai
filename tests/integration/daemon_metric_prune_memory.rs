#![cfg(target_os = "linux")]

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::metrics::db::MetricsDatabase;
use rusqlite::params;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OVERSIZED_METRIC_BYTES: usize = 48 * 1_024 * 1_024;

fn codex_checkpoint(repo: &TestRepo, file_path: &Path, hook_event_name: &str) {
    let hook_input = json!({
        "session_id": "metric-prune-memory-session",
        "cwd": repo.canonical_path().to_string_lossy(),
        "hook_event_name": hook_event_name,
        "tool_name": "apply_patch",
        "tool_use_id": "metric-prune-memory-edit",
        "model": "gpt-5",
        "tool_input": {
            "patch": format!("*** Update File: {}\n", file_path.to_string_lossy())
        },
    })
    .to_string();

    repo.git_ai(&["checkpoint", "codex", "--hook-input", &hook_input])
        .unwrap();
}

fn daemon_hwm_kib(repo: &TestRepo) -> u64 {
    let pid = repo.daemon_pid().expect("test repo should own a daemon");
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmHWM:")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse().ok())
        })
        .expect("daemon status should include VmHWM")
}

fn wait_for_metric_database(metrics_db_path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if MetricsDatabase::open_at_path(metrics_db_path).is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "metrics database was not initialized"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_row_deletion(metrics_db_path: &std::path::Path, row_id: i64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let conn = git_ai::sqlite::open_with_memory_limits(metrics_db_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM metrics WHERE id = ?1",
                params![row_id],
                |row| row.get(0),
            )
            .unwrap();
        if count == 0 {
            return;
        }
        assert!(Instant::now() < deadline, "old metric row was not pruned");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn cached_metric_pruning_does_not_materialize_oversized_json() {
    let metrics_dir = tempfile::tempdir().unwrap();
    let metrics_db_path = metrics_dir.path().join("metrics.db");
    let metrics_db_path_arg = metrics_db_path.to_string_lossy().to_string();
    let repo = TestRepo::new_with_daemon_env(&[(
        "GIT_AI_TEST_METRICS_DB_PATH",
        metrics_db_path_arg.as_str(),
    )]);
    let file_path = repo.path().join("tracked.txt");

    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);
    wait_for_metric_database(&metrics_db_path);

    let baseline_hwm_kib = daemon_hwm_kib(&repo);
    let old_event_ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(366 * 24 * 60 * 60);
    let oversized_json = format!(r#"{{"payload":"{}"}}"#, "x".repeat(OVERSIZED_METRIC_BYTES));
    let conn = git_ai::sqlite::open_with_memory_limits(&metrics_db_path).unwrap();
    conn.execute(
        "INSERT INTO metrics (event_json, delivered_ts, event_ts, event_kind) \
         VALUES (?1, ?2, ?3, 1)",
        params![oversized_json, old_event_ts as i64, old_event_ts as i64],
    )
    .unwrap();
    drop(oversized_json);
    let oversized_row_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT OR REPLACE INTO schema_metadata (key, value) \
         VALUES ('metrics_last_prune_ts', '0')",
        [],
    )
    .unwrap();
    drop(conn);

    codex_checkpoint(&repo, &file_path, "PreToolUse");
    fs::write(&file_path, "base\nai line\n").unwrap();
    codex_checkpoint(&repo, &file_path, "PostToolUse");
    repo.stage_all_and_commit("AI commit triggers metric pruning")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
    wait_for_row_deletion(&metrics_db_path, oversized_row_id);

    let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm_kib);
    assert!(
        hwm_growth_kib < 32 * 1_024,
        "cached metric pruning grew daemon HWM by {hwm_growth_kib} KiB"
    );
}
