use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use chrono::{DateTime, Utc};
use git_ai::metrics::{
    PosEncoded, SessionEventValues, attrs::attr_pos, db::MetricsDatabase, types::MetricEventId,
};
use git_ai::streams::{agent::Agent, agents::OpenCodeAgent, watermark::TimestampWatermark};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::time::{Duration, Instant};

fn assert_opencode_v2_transcript_and_checkpoint(
    with_legacy_tables: bool,
    separate_parts: bool,
    message_count: usize,
) {
    let storage = tempfile::tempdir().unwrap();
    let db_path = storage.path().join("opencode.db");
    if with_legacy_tables {
        fs::copy(
            crate::test_utils::fixture_path("opencode-sqlite/opencode.db"),
            &db_path,
        )
        .unwrap();
    }
    let conn = git_ai::sqlite::open_with_memory_limits(&db_path).unwrap();
    // Mirrors OpenCode v2.1.3/v2.2.5 session/sql.ts: id and type live outside
    // data; assistant content may be inline or in separate session_part rows.
    conn.execute_batch(
        "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT, model TEXT);
         CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
             type TEXT NOT NULL, seq INTEGER NOT NULL, time_created INTEGER NOT NULL,
             time_updated INTEGER NOT NULL, data TEXT NOT NULL);",
    )
    .unwrap();
    // The selected model is optional until explicitly chosen. Once selected,
    // it takes precedence over a previous assistant message after a switch.
    let selected_model =
        with_legacy_tables.then(|| json!({"providerID": "openai", "id": "gpt-5"}).to_string());
    conn.execute(
        "INSERT INTO session_v2 VALUES ('ses_v2', 'ses_parent', ?1)",
        [selected_model],
    )
    .unwrap();
    let mut message = json!({
        "agent": "build",
        "model": {"providerID": "openai", "id": if with_legacy_tables { "previous-model" } else { "gpt-5" }},
        "time": {"created": 1_700_000_000_000_i64},
        "content": [{
            "type": "tool", "id": "call_v2", "name": "edit",
            "state": {"status": "completed", "input": {"path": "example.ts"},
                "content": [{"type": "text", "text": "Updated example.ts"}]},
            "time": {"created": 1_700_000_000_000_i64}
        }]
    });
    let mut tool_part = message["content"][0].clone();
    let completed_tool_part = tool_part.clone();
    if separate_parts {
        tool_part["state"] = json!({
            "status": "running", "input": {"path": "example.ts"}, "metadata": {}
        });
        conn.execute_batch(
            "CREATE TABLE session_part (message_id TEXT NOT NULL, session_id TEXT NOT NULL,
             ordinal INTEGER NOT NULL, type TEXT NOT NULL, call_id TEXT, time_created INTEGER NOT NULL,
             time_updated INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY (message_id, ordinal));",
        ).unwrap();
        conn.execute(
            "INSERT INTO session_part VALUES ('msg_v2', 'ses_v2', 0, 'tool', 'call_v2', ?1, ?1, ?2)",
            rusqlite::params![1_700_000_000_000_i64, tool_part.to_string()],
        ).unwrap();
        message["content"] = json!([]);
    }
    conn.execute(
        "INSERT INTO session_message VALUES ('msg_v2', 'ses_v2', 'assistant', 1, ?1, ?1, ?2)",
        rusqlite::params![1_700_000_000_000_i64, message.to_string()],
    )
    .unwrap();
    drop(conn);

    let metrics_path = storage.path().join("metrics.db");
    let mut repo = TestRepo::new_with_daemon_env(&[(
        "GIT_AI_TEST_METRICS_DB_PATH",
        metrics_path.to_str().unwrap(),
    )]);
    repo.patch_git_ai_config(|patch| patch.exclude_prompts_in_repositories = Some(vec![]));
    let path = repo.canonical_path().join("example.ts");
    fs::write(&path, "// baseline\n").unwrap();
    repo.stage_all_and_commit("Baseline").unwrap();
    let mut file = repo.filename("example.ts");
    file.assert_committed_lines(crate::lines!["// baseline".unattributed_human()]);

    let reader = OpenCodeAgent::new();
    let batch = reader
        .read_incremental(
            &db_path,
            Box::new(TimestampWatermark::new(DateTime::<Utc>::UNIX_EPOCH)),
            "ses_v2",
        )
        .unwrap();
    assert_eq!(
        batch.events.len(),
        1,
        "V2 messages must not be read from the empty legacy session"
    );
    assert_eq!(
        batch.events[0]["message"]["data"], message,
        "Preserve the native V2 transcript"
    );
    assert_eq!(batch.events[0]["message"]["type"], "assistant");
    assert_eq!(batch.events[0]["message"]["seq"], 1);
    if separate_parts {
        assert_eq!(batch.events[0]["parts"][0]["data"], tool_part);
        assert_eq!(batch.events[0]["parts"][0]["ordinal"], 0);
        assert_eq!(batch.events[0]["parts"][0]["call_id"], "call_v2");
    }
    assert_eq!(
        reader.extract_event_ids(&batch.events[0]),
        (Some("msg_v2".into()), None, Some("call_v2".into()))
    );
    let unchanged = reader
        .read_incremental(&db_path, batch.new_watermark, "ses_v2")
        .unwrap();
    assert!(unchanged.events.is_empty());
    if separate_parts {
        // OpenCode's projector advances the parent message timestamp in the
        // same transaction as its part update when a running tool completes.
        let completed_at = 1_700_000_000_100_i64;
        let mut completed_tool_part = completed_tool_part;
        completed_tool_part["time"]["completed"] = json!(completed_at);
        let mut conn = git_ai::sqlite::open_with_memory_limits(&db_path).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "UPDATE session_message SET time_updated = ?1 WHERE id = 'msg_v2'",
            [completed_at],
        )
        .unwrap();
        tx.execute(
            "UPDATE session_part SET time_updated = ?1, data = ?2 WHERE message_id = 'msg_v2' AND ordinal = 0",
            rusqlite::params![completed_at, completed_tool_part.to_string()],
        )
        .unwrap();
        tx.commit().unwrap();
        drop(conn);

        let completed = reader
            .read_incremental(&db_path, unchanged.new_watermark, "ses_v2")
            .unwrap();
        assert_eq!(completed.events.len(), 1);
        assert_eq!(completed.events[0]["message"]["data"], message);
        assert_eq!(completed.events[0]["message"]["time_updated"], completed_at);
        assert_eq!(completed.events[0]["parts"][0]["data"], completed_tool_part);
        assert_eq!(
            completed.events[0]["parts"][0]["time_updated"],
            completed_at
        );
        assert_eq!(
            reader.extract_event_ids(&completed.events[0]),
            (Some("msg_v2".into()), None, Some("call_v2".into()))
        );
        assert!(
            reader
                .read_incremental(&db_path, completed.new_watermark, "ses_v2")
                .unwrap()
                .events
                .is_empty()
        );
    }
    if with_legacy_tables {
        let legacy = reader
            .read_incremental(
                &db_path,
                Box::new(TimestampWatermark::new(DateTime::<Utc>::UNIX_EPOCH)),
                "test-session-123",
            )
            .unwrap();
        assert!(
            !legacy.events.is_empty(),
            "V1 sessions in an upgraded database remain readable"
        );
        assert!(
            legacy
                .events
                .iter()
                .any(|event| event.get("parts").is_some())
        );
    }

    let mut expected_event_ids = BTreeSet::from(["msg_v2".to_string()]);
    if message_count > 1 {
        // A session can exceed the reader's 1000-row batch while many messages
        // share a millisecond. The daemon must persist the entire tied group.
        let mut conn = git_ai::sqlite::open_with_memory_limits(&db_path).unwrap();
        let tx = conn.transaction().unwrap();
        for index in 1..message_count {
            let id = format!("msg_v2_{index:04}");
            tx.execute(
                "INSERT INTO session_message VALUES (?1, 'ses_v2', 'assistant', ?2, ?3, ?3, ?4)",
                rusqlite::params![id, index + 1, 1_700_000_000_000_i64, message.to_string()],
            )
            .unwrap();
            expected_event_ids.insert(id);
        }
        tx.commit().unwrap();
    }

    let checkpoint = |event: &str| {
        let hook_input = json!({
            "hook_event_name": event, "session_id": "ses_v2", "tool_use_id": "call_v2",
            "cwd": repo.canonical_path(), "tool_name": "edit", "tool_input": {"path": path}
        })
        .to_string();
        repo.git_ai_with_env(
            &["checkpoint", "opencode", "--hook-input", &hook_input],
            &[(
                "GIT_AI_OPENCODE_STORAGE_PATH",
                storage.path().to_str().unwrap(),
            )],
        )
        .unwrap();
    };
    checkpoint("PreToolUse");
    fs::write(&path, "// baseline\n// V2 edit\n").unwrap();
    checkpoint("PostToolUse");
    let commit = repo.stage_all_and_commit("OpenCode V2 edit").unwrap();
    file.assert_committed_lines(crate::lines![
        "// baseline".unattributed_human(),
        "// V2 edit".ai()
    ]);
    let session = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .unwrap();
    assert_eq!(session.agent_id.model, "gpt-5");
    assert_eq!(session.agent_id.tool, "opencode");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let metrics = MetricsDatabase::open_at_path(&metrics_path).unwrap();
        let records = metrics
            .get_metric_history(0, None, &[MetricEventId::SessionEvent as u16])
            .unwrap();
        let session_records: Vec<_> = records
            .iter()
            .filter(|record| {
                record
                    .event
                    .attrs
                    .get(&attr_pos::EXTERNAL_SESSION_ID.to_string())
                    .and_then(|v| v.as_str())
                    == Some("ses_v2")
            })
            .collect();
        if session_records.len() == message_count {
            let event_ids: BTreeSet<_> = session_records
                .iter()
                .map(|record| {
                    SessionEventValues::from_sparse(&record.event.values)
                        .external_event_id
                        .unwrap()
                })
                .collect();
            assert_eq!(event_ids, expected_event_ids);
            let record = session_records[0];
            assert_eq!(
                record.event.timestamp, 1_700_000_000,
                "Session events use the message creation time, not the database file time"
            );
            assert_eq!(
                record
                    .event
                    .attrs
                    .get(&attr_pos::EXTERNAL_PARENT_SESSION_ID.to_string())
                    .and_then(|v| v.as_str()),
                Some("ses_parent")
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "All V2 transcript messages should reach the daemon's session events: received {} of {message_count}",
            session_records.len()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn test_opencode_v2_transcript_checkpoint_and_parent() {
    assert_opencode_v2_transcript_and_checkpoint(false, false, 1);
}

#[test]
fn test_opencode_v2_transcript_with_legacy_tables() {
    assert_opencode_v2_transcript_and_checkpoint(true, false, 1);
}

#[test]
fn test_opencode_v2_transcript_with_separate_parts() {
    assert_opencode_v2_transcript_and_checkpoint(true, true, 1);
}

#[test]
fn test_opencode_v2_transcript_tied_timestamp_pagination() {
    assert_opencode_v2_transcript_and_checkpoint(false, false, 1001);
}

#[test]
fn test_opencode_message_timestamp_fallbacks() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let metadata = file.as_file().metadata().unwrap();
    let reader = OpenCodeAgent::new();
    let cases = [
        (
            json!({"message": {
                "time_created": 1_700_000_000_123_u64,
                "time_updated": 1_700_000_100_456_u64,
                "data": {"time": {"created": 1_600_000_000_000_u64}}
            }}),
            1_700_000_000,
        ),
        (
            json!({"message": {"data": {"time": {"created": 1_700_000_001_999_u64}}}}),
            1_700_000_001,
        ),
        (
            json!({"message": {
                "time_created": "invalid",
                "data": {"time": {"created": 1_700_000_002_000_u64}}
            }}),
            1_700_000_002,
        ),
        (json!({"timestamp": "2023-11-14T22:13:23Z"}), 1_700_000_003),
    ];
    for (event, expected) in cases {
        for is_first_event in [true, false] {
            assert_eq!(
                reader.extract_event_timestamp(&event, &metadata, is_first_event),
                expected,
                "Unexpected message timestamp for {event}"
            );
        }
    }
    let missing_timestamp = json!({"message": {"data": {}}});
    for is_first_event in [true, false] {
        assert_eq!(
            reader.extract_event_timestamp(&missing_timestamp, &metadata, is_first_event),
            git_ai::streams::agent::file_time_fallback(&metadata, is_first_event),
            "Messages without a creation time retain the file metadata fallback"
        );
    }
}
