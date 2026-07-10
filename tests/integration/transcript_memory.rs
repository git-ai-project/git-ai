use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::types::MetricEventId;
use git_ai::metrics::{PosEncoded, SessionEventValues};
use git_ai::streams::agent::Agent;
use git_ai::streams::agents::ClaudeAgent;
use git_ai::streams::watermark::ByteOffsetWatermark;
use serde_json::json;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

const EVENT_PAYLOAD_BYTES: usize = 512 * 1024;

fn write_jsonl_event(writer: &mut impl Write, uuid: &str, payload_bytes: usize) {
    serde_json::to_writer(
        &mut *writer,
        &json!({
            "uuid": uuid,
            "timestamp": "2026-07-10T00:00:00Z",
            "message": { "content": "x".repeat(payload_bytes) },
        }),
    )
    .unwrap();
    writer.write_all(b"\n").unwrap();
}

fn wait_for_test_session_events(db_path: &Path) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let db = MetricsDatabase::open_at_path(db_path).expect("metrics database should open");
        let events = db
            .get_metric_history(0, None, &[MetricEventId::SessionEvent as u16])
            .expect("session metrics should load")
            .into_iter()
            .filter_map(|record| {
                SessionEventValues::from_sparse(&record.event.values)
                    .raw_json
                    .get("uuid")
                    .and_then(|uuid| uuid.as_str())
                    .filter(|uuid| matches!(*uuid, "oversized" | "kept"))
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();

        if events.iter().any(|uuid| uuid == "kept") {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "valid event after oversized record was not persisted"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn transcript_daemon_skips_oversized_record_and_keeps_following_event() {
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

    let transcript_path = metrics_dir.path().join("session.jsonl");
    fs::write(&transcript_path, "").unwrap();
    let hook_input = |hook_event_name: &str| {
        json!({
            "cwd": repo.canonical_path().to_string_lossy(),
            "hook_event_name": hook_event_name,
            "session_id": "transcript-memory-session",
            "tool_name": "Write",
            "tool_use_id": "transcript-memory-tool-use",
            "transcript_path": transcript_path.to_string_lossy(),
            "tool_input": { "file_path": file_path.to_string_lossy() },
        })
        .to_string()
    };

    let pre_hook = hook_input("PreToolUse");
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &pre_hook])
        .unwrap();
    fs::write(&file_path, "base\nai line\n").unwrap();

    let transcript = File::create(&transcript_path).unwrap();
    let mut transcript = BufWriter::new(transcript);
    write_jsonl_event(&mut transcript, "oversized", 2 * 1024 * 1024);
    write_jsonl_event(&mut transcript, "kept", 16);
    transcript.flush().unwrap();

    let post_hook = hook_input("PostToolUse");
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &post_hook])
        .unwrap();

    assert_eq!(wait_for_test_session_events(&metrics_db_path), ["kept"]);

    repo.stage_all_and_commit("AI edit").unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}

#[test]
fn transcript_reader_limits_batch_by_bytes_not_only_event_count() {
    let temp_dir = tempfile::tempdir().unwrap();
    let transcript_path = temp_dir.path().join("session.jsonl");
    let transcript = File::create(&transcript_path).unwrap();
    let mut transcript = BufWriter::new(transcript);
    for index in 0..24 {
        write_jsonl_event(
            &mut transcript,
            &format!("event-{index}"),
            EVENT_PAYLOAD_BYTES,
        );
    }
    transcript.flush().unwrap();

    let agent = ClaudeAgent::new();
    let first = agent
        .read_incremental(
            &transcript_path,
            Box::new(ByteOffsetWatermark::new(0)),
            "transcript-memory-session",
        )
        .unwrap();
    assert!(
        first.events.len() < 24,
        "the reader loaded the entire 12 MiB transcript into one batch"
    );
    assert!(!first.events.is_empty());

    let second = agent
        .read_incremental(
            &transcript_path,
            first.new_watermark,
            "transcript-memory-session",
        )
        .unwrap();
    assert!(!second.events.is_empty());
    assert_eq!(first.events.len() + second.events.len(), 24);
}
