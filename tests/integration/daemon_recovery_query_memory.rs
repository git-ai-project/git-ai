use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::generate_session_id;
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::{EventAttributes, MetricEvent, PosEncoded, SessionEventValues};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

const RECOVERY_CANDIDATE_FLOOD: usize = 9;

fn file_mtime_secs(path: &Path) -> u32 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .min(u32::MAX as u64) as u32
}

fn insert_session_event_flood(db_path: &Path, event_ts: u32) {
    let events = (0..RECOVERY_CANDIDATE_FLOOD)
        .map(|index| {
            let external_session_id = format!("overload-session-{index}");
            let external_tool_use_id = format!("overload-tool-use-{index}");
            let session_id = generate_session_id(&external_session_id, "codex");
            let values = SessionEventValues::with_ids(
                json!({
                    "type": "assistant",
                    "session_id": external_session_id,
                }),
                Some(format!("overload-event-{index}")),
                None,
                Some(external_tool_use_id),
            );
            let attrs = EventAttributes::with_version("test")
                .tool("codex")
                .model("gpt-5")
                .external_session_id(&external_session_id)
                .session_id(&session_id)
                .trace_id(format!("overload-trace-{index}"))
                .repo_url("https://github.com/acme/recovery-query-memory");
            serde_json::to_string(&MetricEvent::from_values_with_timestamp(
                values,
                attrs.to_sparse(),
                Some(event_ts),
            ))
            .unwrap()
        })
        .collect::<Vec<_>>();

    let mut db = MetricsDatabase::open_at_path(db_path).unwrap();
    db.insert_events(&events).unwrap();
}

#[test]
fn recovery_candidate_flood_fails_closed_and_next_commit_stays_attributed() {
    let metrics_dir = tempfile::tempdir().unwrap();
    let metrics_db_path = metrics_dir.path().join("metrics.db");
    let metrics_db_path_arg = metrics_db_path.to_string_lossy().to_string();
    let repo = TestRepo::new_with_daemon_env(&[
        ("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path_arg.as_str()),
        ("GIT_AI_TEST_ATTRIBUTION_RECOVERY_CANDIDATE_LIMIT", "8"),
    ]);
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/recovery-query-memory.git",
    ])
    .unwrap();
    let file_path = repo.path().join("tracked.txt");

    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    fs::write(&file_path, "base\nuncheckpointed line\n").unwrap();
    insert_session_event_flood(&metrics_db_path, file_mtime_secs(&file_path));
    repo.stage_all_and_commit("Commit under recovery query pressure")
        .unwrap();
    file.assert_committed_lines(lines![
        "base".unattributed_human(),
        "uncheckpointed line".unattributed_human(),
    ]);

    fs::write(
        &file_path,
        "base\nuncheckpointed line\ncheckpointed AI line\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI commit after recovery query pressure")
        .unwrap();
    file.assert_committed_lines(lines![
        "base".unattributed_human(),
        "uncheckpointed line".unattributed_human(),
        "checkpointed AI line".ai(),
    ]);
}
