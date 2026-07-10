use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::types::MetricEventId;
use git_ai::metrics::{PosEncoded, SessionEventValues};
use serde_json::json;
use std::fs;
use std::time::{Duration, Instant};

#[test]
fn malformed_transcript_log_burst_keeps_daemon_responsive() {
    let metrics_dir = tempfile::tempdir().unwrap();
    let metrics_db_path = metrics_dir.path().join("metrics.db");
    let metrics_db_path_str = metrics_db_path.to_string_lossy().to_string();
    let repo = TestRepo::new_with_daemon_env(&[
        ("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path_str.as_str()),
        ("GIT_AI_API_BASE_URL", "http://127.0.0.1:9"),
    ]);

    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    let transcript_path = metrics_dir.path().join("log-burst-session.jsonl");
    fs::write(&transcript_path, "").unwrap();
    let hook_input = |hook_event_name: &str| {
        json!({
            "cwd": repo.canonical_path().to_string_lossy(),
            "hook_event_name": hook_event_name,
            "session_id": "daemon-log-memory-session",
            "tool_name": "Write",
            "tool_use_id": "daemon-log-memory-tool-use",
            "transcript_path": transcript_path.to_string_lossy(),
            "tool_input": { "file_path": file_path.to_string_lossy() },
        })
        .to_string()
    };

    let pre_hook = hook_input("PreToolUse");
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &pre_hook])
        .unwrap();
    fs::write(&file_path, "base\nai line\n").unwrap();

    let mut transcript = "not-json\n".repeat(10_000);
    transcript.push_str(
        &json!({
            "uuid": "kept-after-log-burst",
            "timestamp": "2026-07-10T00:00:00Z",
            "message": { "content": "valid" },
        })
        .to_string(),
    );
    transcript.push('\n');
    fs::write(&transcript_path, transcript).unwrap();

    let post_hook = hook_input("PostToolUse");
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &post_hook])
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let db = MetricsDatabase::open_at_path(&metrics_db_path).unwrap();
        let persisted = db
            .get_metric_history(0, None, &[MetricEventId::SessionEvent as u16])
            .unwrap()
            .into_iter()
            .any(|record| {
                SessionEventValues::from_sparse(&record.event.values)
                    .raw_json
                    .get("uuid")
                    .and_then(|uuid| uuid.as_str())
                    == Some("kept-after-log-burst")
            });
        if persisted {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "valid event after malformed transcript burst was not persisted"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    repo.git_ai(&["status", "--json"])
        .expect("daemon should remain responsive after log burst");
    repo.stage_all_and_commit("AI edit after log burst")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
