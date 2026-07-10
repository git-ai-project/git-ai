use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::types::MetricEventId;
use git_ai::metrics::{PosEncoded, SessionEventValues};
use serde_json::json;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::{Duration, Instant};

const EVENT_COUNT: usize = 24;

#[test]
fn transcript_checkpoint_burst_coalesces_without_losing_events() {
    let metrics_dir = tempfile::tempdir().unwrap();
    let metrics_db_path = metrics_dir.path().join("metrics.db");
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

    let transcript_path = metrics_dir.path().join("queue-session.jsonl");
    fs::write(&transcript_path, "").unwrap();
    let hook_input = |hook_event_name: &str| {
        json!({
            "cwd": repo.canonical_path().to_string_lossy(),
            "hook_event_name": hook_event_name,
            "session_id": "transcript-queue-session",
            "tool_name": "Write",
            "tool_use_id": "transcript-queue-tool-use",
            "transcript_path": transcript_path.to_string_lossy(),
            "tool_input": { "file_path": file_path.to_string_lossy() },
        })
        .to_string()
    };

    let pre_hook = hook_input("PreToolUse");
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &pre_hook])
        .unwrap();
    fs::write(&file_path, "base\nai line\n").unwrap();

    let post_hook = hook_input("PostToolUse");
    for index in 0..EVENT_COUNT {
        let mut transcript = OpenOptions::new()
            .append(true)
            .open(&transcript_path)
            .unwrap();
        serde_json::to_writer(
            &mut transcript,
            &json!({
                "uuid": format!("queue-event-{index}"),
                "timestamp": "2026-07-10T00:00:00Z",
                "message": { "content": "x".repeat(64 * 1024) },
            }),
        )
        .unwrap();
        transcript.write_all(b"\n").unwrap();
        transcript.flush().unwrap();

        repo.git_ai(&["checkpoint", "claude", "--hook-input", &post_hook])
            .unwrap();
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    let event_ids = loop {
        let db = MetricsDatabase::open_at_path(&metrics_db_path).unwrap();
        let event_ids = db
            .get_metric_history(0, None, &[MetricEventId::SessionEvent as u16])
            .unwrap()
            .into_iter()
            .filter_map(|record| {
                SessionEventValues::from_sparse(&record.event.values)
                    .raw_json
                    .get("uuid")
                    .and_then(|uuid| uuid.as_str())
                    .filter(|uuid| uuid.starts_with("queue-event-"))
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        if event_ids.len() == EVENT_COUNT {
            break event_ids;
        }
        assert!(
            Instant::now() < deadline,
            "expected {EVENT_COUNT} queued transcript events, found {}",
            event_ids.len()
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    assert_eq!(event_ids.iter().collect::<HashSet<_>>().len(), EVENT_COUNT);

    repo.stage_all_and_commit("AI edit after checkpoint burst")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
