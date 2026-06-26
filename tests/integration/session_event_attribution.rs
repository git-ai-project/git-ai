use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::generate_session_id;
use git_ai::authorship::working_log::AgentId;
use git_ai::daemon::bash_history_db::{BashCallEnd, BashCallStart, BashHistoryDatabase};
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::{EventAttributes, MetricEvent, PosEncoded, SessionEventValues};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

fn isolated_metrics_db_path() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("failed to create isolated metrics db dir");
    let path = dir.path().join("metrics.db");
    (dir, path.to_string_lossy().to_string())
}

fn file_mtime_secs(path: &Path) -> u32 {
    fs::metadata(path)
        .expect("file metadata should be readable")
        .modified()
        .expect("file mtime should be readable")
        .duration_since(UNIX_EPOCH)
        .expect("file mtime should be after epoch")
        .as_secs()
        .min(u32::MAX as u64) as u32
}

fn insert_session_event(
    db_path: &str,
    event_ts: u32,
    external_session_id: &str,
    external_tool_use_id: &str,
    repo_url: Option<&str>,
) -> String {
    let tool = "codex";
    let session_id = generate_session_id(external_session_id, tool);
    let values = SessionEventValues::with_ids(
        json!({
            "type": "assistant",
            "session_id": external_session_id,
        }),
        Some(format!("event-{external_tool_use_id}")),
        None,
        Some(external_tool_use_id.to_string()),
    );
    let mut attrs = EventAttributes::with_version("test")
        .tool(tool)
        .model("gpt-5")
        .external_session_id(external_session_id)
        .session_id(&session_id)
        .trace_id(format!("trace-{external_tool_use_id}"));
    if let Some(repo_url) = repo_url {
        attrs = attrs.repo_url(repo_url);
    }
    let event = MetricEvent::from_values_with_timestamp(values, attrs.to_sparse(), Some(event_ts));
    let event_json = serde_json::to_string(&event).expect("metric event should serialize");

    let mut db = MetricsDatabase::open_at_path(Path::new(db_path))
        .expect("metrics db should open at isolated path");
    db.insert_events(&[event_json])
        .expect("session event should insert");

    session_id
}

fn isolated_bash_history_db_path() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("failed to create isolated bash history db dir");
    let path = dir.path().join("bash-history.db");
    (dir, path.to_string_lossy().to_string())
}

fn insert_bash_call(
    db_path: &str,
    repo_work_dir: &str,
    timestamp_secs: u32,
    external_session_id: &str,
    tool_use_id: &str,
) -> String {
    let tool = "codex";
    let session_id = generate_session_id(external_session_id, tool);
    let mut db = BashHistoryDatabase::open_at_path(Path::new(db_path))
        .expect("bash history db should open at isolated path");
    let start_ns = u128::from(timestamp_secs).saturating_mul(1_000_000_000);
    let end_ns = start_ns.saturating_add(1_000_000_000);
    let agent_id = AgentId {
        tool: tool.to_string(),
        id: external_session_id.to_string(),
        model: "gpt-5".to_string(),
    };
    db.record_start(&BashCallStart {
        original_cwd: repo_work_dir.to_string(),
        repo_work_dir: Some(repo_work_dir.to_string()),
        repo_discovery_error: None,
        session_id: external_session_id.to_string(),
        tool_use_id: tool_use_id.to_string(),
        agent_id: agent_id.clone(),
        start_trace_id: format!("trace-start-{tool_use_id}"),
        started_at_ns: start_ns,
        command: Some("codex exec".to_string()),
        metadata: HashMap::new(),
    })
    .expect("bash start should insert");
    db.record_end(&BashCallEnd {
        original_cwd: repo_work_dir.to_string(),
        repo_work_dir: Some(repo_work_dir.to_string()),
        repo_discovery_error: None,
        session_id: external_session_id.to_string(),
        tool_use_id: tool_use_id.to_string(),
        agent_id,
        start_trace_id: Some(format!("trace-start-{tool_use_id}")),
        end_trace_id: format!("trace-end-{tool_use_id}"),
        started_at_ns: Some(start_ns),
        ended_at_ns: end_ns,
        command: Some("codex exec".to_string()),
        metadata: HashMap::new(),
    })
    .expect("bash end should insert");

    session_id
}

#[test]
fn test_session_event_recovery_attributes_uncheckpointed_repo_linked_commit() {
    let (_metrics_db_dir, metrics_db_path) = isolated_metrics_db_path();
    let repo =
        TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path.as_str())]);
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/session-event-recovery.git",
    ])
    .expect("remote add should succeed");
    repo.git(&["commit", "--allow-empty", "-m", "initial"])
        .expect("initial empty commit should succeed");

    let file_path = repo.path().join("generated.txt");
    fs::write(&file_path, "generated by AI before hooks\n").unwrap();
    let recovered_session_id = insert_session_event(
        &metrics_db_path,
        file_mtime_secs(&file_path),
        "external-repo-linked-session",
        "tool-use-repo-linked",
        Some("https://github.com/acme/session-event-recovery"),
    );

    let commit = repo
        .stage_all_and_commit("Recover uncheckpointed AI")
        .expect("commit should succeed");

    let mut file = repo.filename("generated.txt");
    file.assert_committed_lines(lines!["generated by AI before hooks".ai()]);
    assert!(
        commit
            .authorship_log
            .metadata
            .sessions
            .contains_key(&recovered_session_id),
        "recovered note should include the session-event session record"
    );
}

#[test]
fn test_session_event_recovery_does_not_override_nearby_bash_candidate() {
    let (_metrics_db_dir, metrics_db_path) = isolated_metrics_db_path();
    let (_bash_db_dir, bash_db_path) = isolated_bash_history_db_path();
    let env = [
        ("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path.as_str()),
        ("GIT_AI_TEST_BASH_CHECKPOINT_DB_PATH", bash_db_path.as_str()),
    ];
    let repo = TestRepo::new_with_daemon_env(&env);
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/session-event-recovery.git",
    ])
    .expect("remote add should succeed");
    repo.git(&["commit", "--allow-empty", "-m", "initial"])
        .expect("initial empty commit should succeed");

    let file_path = repo.path().join("generated-with-bash-noise.txt");
    fs::write(&file_path, "generated by inner codex\n").unwrap();
    let file_ts = file_mtime_secs(&file_path);
    let recovered_session_id = insert_session_event(
        &metrics_db_path,
        file_ts,
        "external-inner-session",
        "tool-use-inner",
        Some("https://github.com/acme/session-event-recovery"),
    );
    let bash_session_id = insert_bash_call(
        &bash_db_path,
        repo.canonical_path().to_string_lossy().as_ref(),
        file_ts,
        "external-outer-bash-session",
        "tool-use-outer-bash",
    );

    let commit = repo
        .stage_all_and_commit("Nearby bash attribution runs before session event")
        .expect("commit should succeed");

    let mut file = repo.filename("generated-with-bash-noise.txt");
    file.assert_committed_lines(lines!["generated by inner codex".ai()]);
    assert!(
        !commit
            .authorship_log
            .metadata
            .sessions
            .contains_key(&recovered_session_id),
        "session-event recovery should only see lines still unknown after bash recovery"
    );
    assert!(
        commit
            .authorship_log
            .metadata
            .sessions
            .contains_key(&bash_session_id),
        "bash recovery should retain first pass over nearby bash candidates"
    );
}

#[test]
fn test_session_event_recovery_does_not_override_known_human_checkpoint() {
    let (_metrics_db_dir, metrics_db_path) = isolated_metrics_db_path();
    let repo =
        TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path.as_str())]);
    repo.git(&["commit", "--allow-empty", "-m", "initial"])
        .expect("initial empty commit should succeed");

    let file_path = repo.path().join("human.txt");
    fs::write(&file_path, "typed by a human\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "human.txt"])
        .expect("known-human checkpoint should succeed");
    let recovered_session_id = insert_session_event(
        &metrics_db_path,
        file_mtime_secs(&file_path),
        "external-human-nearby-session",
        "tool-use-human-nearby",
        None,
    );

    let commit = repo
        .stage_all_and_commit("Known human stays human")
        .expect("commit should succeed");

    let mut file = repo.filename("human.txt");
    file.assert_committed_lines(lines!["typed by a human".human()]);
    assert!(
        !commit
            .authorship_log
            .metadata
            .sessions
            .contains_key(&recovered_session_id),
        "nearby session event must not be used when explicit human attribution exists"
    );
}

#[test]
fn test_session_event_recovery_ignores_events_outside_window() {
    let (_metrics_db_dir, metrics_db_path) = isolated_metrics_db_path();
    let repo =
        TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path.as_str())]);
    repo.git(&["commit", "--allow-empty", "-m", "initial"])
        .expect("initial empty commit should succeed");

    let file_path = repo.path().join("outside.txt");
    fs::write(&file_path, "outside the window\n").unwrap();
    let file_ts = file_mtime_secs(&file_path);
    let recovered_session_id = insert_session_event(
        &metrics_db_path,
        file_ts.saturating_sub(10),
        "external-outside-window-session",
        "tool-use-outside-window",
        None,
    );

    let commit = repo
        .stage_all_and_commit("Outside window stays unknown")
        .expect("commit should succeed");

    let mut file = repo.filename("outside.txt");
    file.assert_committed_lines(lines!["outside the window".unattributed_human()]);
    assert!(
        !commit
            .authorship_log
            .metadata
            .sessions
            .contains_key(&recovered_session_id),
        "outside-window session event must not recover attribution"
    );
}

#[test]
fn test_session_event_recovery_rejects_time_only_sessions() {
    let (_metrics_db_dir, metrics_db_path) = isolated_metrics_db_path();
    let repo =
        TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path.as_str())]);
    repo.git(&["commit", "--allow-empty", "-m", "initial"])
        .expect("initial empty commit should succeed");

    let file_path = repo.path().join("time-only.txt");
    fs::write(&file_path, "time only\n").unwrap();
    let file_ts = file_mtime_secs(&file_path);
    let recovered_session_id = insert_session_event(
        &metrics_db_path,
        file_ts,
        "external-time-only",
        "tool-use-time-only",
        None,
    );

    let commit = repo
        .stage_all_and_commit("Time-only session stays unknown")
        .expect("commit should succeed");

    let mut file = repo.filename("time-only.txt");
    file.assert_committed_lines(lines!["time only".unattributed_human()]);
    assert!(
        !commit
            .authorship_log
            .metadata
            .sessions
            .contains_key(&recovered_session_id),
        "time-only session events must not recover attribution"
    );
}
