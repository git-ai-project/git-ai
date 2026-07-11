use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::generate_session_id;
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::types::MetricEventId;
use git_ai::metrics::{PosEncoded, SessionEventValues};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const EXTERNAL_SESSION_ID: &str = "sqlite-memory-session";

fn create_oversized_opencode_db(path: &Path) {
    let mut conn = git_ai::sqlite::open_with_memory_limits(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT);
         CREATE TABLE message (
             id TEXT PRIMARY KEY,
             session_id TEXT NOT NULL,
             time_created INTEGER NOT NULL,
             time_updated INTEGER NOT NULL,
             data TEXT NOT NULL
         );
         CREATE TABLE part (
             id TEXT PRIMARY KEY,
             message_id TEXT NOT NULL,
             session_id TEXT NOT NULL,
             time_created INTEGER NOT NULL,
             time_updated INTEGER NOT NULL,
             data TEXT NOT NULL
         );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session (id, parent_id) VALUES (?1, NULL)",
        [EXTERNAL_SESSION_ID],
    )
    .unwrap();
    let internal_session_id = generate_session_id(EXTERNAL_SESSION_ID, "opencode");
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES ('sqlite-memory-message', ?1, 1000, 1000, ?2)",
        rusqlite::params![
            internal_session_id,
            json!({"role": "assistant", "modelID": "gpt-5"}).to_string()
        ],
    )
    .unwrap();

    let payload = json!({
        "type": "text",
        "text": "x".repeat(256 * 1024),
    })
    .to_string();
    let transaction = conn.transaction().unwrap();
    {
        let mut statement = transaction
            .prepare(
                "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, 'sqlite-memory-message', ?2, ?3, ?3, ?4)",
            )
            .unwrap();
        for index in 0..40 {
            statement
                .execute(rusqlite::params![
                    format!("sqlite-memory-part-{index}"),
                    internal_session_id,
                    1001 + index,
                    payload,
                ])
                .unwrap();
        }
    }
    transaction.commit().unwrap();
}

fn repair_opencode_db(path: &Path) {
    let conn = git_ai::sqlite::open_with_memory_limits(path).unwrap();
    conn.execute("DELETE FROM part", []).unwrap();
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES ('sqlite-memory-recovery-part', 'sqlite-memory-message', ?1, 1001, 1001, ?2)",
        rusqlite::params![
            generate_session_id(EXTERNAL_SESSION_ID, "opencode"),
            json!({"type": "text", "text": "recovered"}).to_string(),
        ],
    )
    .unwrap();
}

fn wait_for_recovery_event(metrics_db_path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(db) = MetricsDatabase::open_at_path(metrics_db_path)
            && db
                .get_metric_history(0, None, &[MetricEventId::SessionEvent as u16])
                .unwrap_or_default()
                .iter()
                .any(|record| {
                    SessionEventValues::from_sparse(&record.event.values)
                        .raw_json
                        .get("message")
                        .and_then(|message| message.get("id"))
                        .and_then(|value| value.as_str())
                        == Some("sqlite-memory-message")
                })
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "repaired SQLite transcript was not processed"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(target_os = "linux")]
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

#[test]
fn sqlite_transcript_fanout_keeps_daemon_bounded_and_recovers() {
    let fixture = tempfile::tempdir().unwrap();
    let storage_path = fixture.path().join("opencode");
    fs::create_dir_all(&storage_path).unwrap();
    let db_path = storage_path.join("opencode.db");
    create_oversized_opencode_db(&db_path);

    let metrics_db_path = fixture.path().join("metrics.db");
    let metrics_db_path_str = metrics_db_path.to_string_lossy().to_string();
    let repo = TestRepo::new_with_daemon_env(&[(
        "GIT_AI_TEST_METRICS_DB_PATH",
        metrics_db_path_str.as_str(),
    )]);
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    let hook_input = |hook_event_name: &str| {
        json!({
            "hook_event_name": hook_event_name,
            "session_id": EXTERNAL_SESSION_ID,
            "cwd": repo.canonical_path(),
            "tool_name": "edit",
            "tool_input": {"filePath": file_path},
        })
        .to_string()
    };
    let storage_path_str = storage_path.to_string_lossy().to_string();
    let pre_hook = hook_input("PreToolUse");
    repo.git_ai_with_env(
        &["checkpoint", "opencode", "--hook-input", &pre_hook],
        &[("GIT_AI_OPENCODE_STORAGE_PATH", storage_path_str.as_str())],
    )
    .unwrap();
    fs::write(&file_path, "base\nai line\n").unwrap();

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);
    let post_hook = hook_input("PostToolUse");
    repo.git_ai_with_env(
        &["checkpoint", "opencode", "--hook-input", &post_hook],
        &[("GIT_AI_OPENCODE_STORAGE_PATH", storage_path_str.as_str())],
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(2));

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        assert!(
            hwm_growth_kib < 48 * 1024,
            "SQLite transcript fanout grew daemon HWM by {hwm_growth_kib} KiB"
        );
    }

    repair_opencode_db(&db_path);
    repo.git_ai_with_env(
        &["checkpoint", "opencode", "--hook-input", &post_hook],
        &[("GIT_AI_OPENCODE_STORAGE_PATH", storage_path_str.as_str())],
    )
    .unwrap();
    wait_for_recovery_event(&metrics_db_path);

    repo.stage_all_and_commit("AI edit after SQLite transcript pressure")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
