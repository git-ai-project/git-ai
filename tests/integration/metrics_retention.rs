use crate::repos::test_repo::TestRepo;
use crate::test_utils::isolated_metrics_db_path;
use git_ai::metrics::db::MetricsDatabase;
use git_ai::sqlite::open_with_memory_limits;
use git_ai::utils::unix_timestamp_now;
use std::path::Path;

#[test]
fn daemon_prunes_existing_metrics_using_configured_retention_days() {
    let (_dir, path) = isolated_metrics_db_path();
    MetricsDatabase::open_at_path(Path::new(&path)).unwrap();
    let conn = open_with_memory_limits(Path::new(&path)).unwrap();
    let now = unix_timestamp_now() as u32;
    let old = now - 8 * 86400;
    let recent = now - 6 * 86400;
    for ts in [old, recent] {
        let event = serde_json::json!({"t": ts, "e": 1, "v": {}, "a": {}});
        conn.execute(
            "INSERT INTO metrics (event_json, event_ts) VALUES (?1, ?2)",
            rusqlite::params![event.to_string(), ts],
        )
        .unwrap();
    }

    let repo = TestRepo::new_with_daemon_env(&[
        ("GIT_AI_TEST_METRICS_DB_PATH", path.as_str()),
        ("GIT_AI_METRICS_RETENTION_DAYS", "7"),
    ]);
    std::fs::write(repo.path().join("example.txt"), "example\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "example.txt"])
        .unwrap();
    repo.git_ai(&["await"]).unwrap();

    let retained = |ts| {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM metrics WHERE event_ts = ?1)",
            [ts],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    };
    assert!(!retained(old), "the eight-day-old metric should be pruned");
    assert!(retained(recent), "the six-day-old metric should be retained");
}
