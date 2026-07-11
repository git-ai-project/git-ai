use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::generate_session_id;
use git_ai::authorship::working_log::AgentId;
use git_ai::daemon::bash_history_db::{BashCallStart, BashHistoryDatabase};
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::{EventAttributes, MetricEvent, PosEncoded, SessionEventValues};
use serde_json::json;
use std::collections::HashMap;
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

fn file_mtime_ns(path: &Path) -> u128 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn insert_session_event_flood(
    db_path: &Path,
    event_ts: u32,
    candidate_count: usize,
    padding_bytes: usize,
) {
    let events = (0..candidate_count)
        .map(|index| {
            let external_session_id = format!("overload-session-{index}");
            let external_tool_use_id = format!("overload-tool-use-{index}");
            let session_id = generate_session_id(&external_session_id, "codex");
            let values = SessionEventValues::with_ids(
                json!({
                    "type": "assistant",
                    "session_id": external_session_id,
                    "padding": "x".repeat(padding_bytes),
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

fn clear_metrics(db_path: &Path) {
    let conn = git_ai::sqlite::open_with_memory_limits(db_path).unwrap();
    conn.execute("DELETE FROM metrics", []).unwrap();
}

fn replace_cached_metric_session_id(db_path: &Path, session_id: &str) {
    let conn = git_ai::sqlite::open_with_memory_limits(db_path).unwrap();
    conn.execute(
        "UPDATE metrics SET session_id = ?1",
        rusqlite::params![session_id],
    )
    .unwrap();
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
fn recovery_candidate_flood_fails_closed_and_next_commit_stays_attributed() {
    let metrics_dir = tempfile::tempdir().unwrap();
    let metrics_db_path = metrics_dir.path().join("metrics.db");
    let bash_db_path = metrics_dir.path().join("bash.db");
    drop(BashHistoryDatabase::open_at_path(&bash_db_path).unwrap());
    let metrics_db_path_arg = metrics_db_path.to_string_lossy().to_string();
    let bash_db_path_arg = bash_db_path.to_string_lossy().to_string();
    let repo = TestRepo::new_with_daemon_env(&[
        ("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path_arg.as_str()),
        (
            "GIT_AI_TEST_BASH_CHECKPOINT_DB_PATH",
            bash_db_path_arg.as_str(),
        ),
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
    insert_session_event_flood(
        &metrics_db_path,
        file_mtime_secs(&file_path),
        RECOVERY_CANDIDATE_FLOOD,
        0,
    );
    repo.stage_all_and_commit("Commit under recovery query pressure")
        .unwrap();
    file.assert_committed_lines(lines![
        "base".unattributed_human(),
        "uncheckpointed line".unattributed_human(),
    ]);

    clear_metrics(&metrics_db_path);
    fs::write(
        &file_path,
        "base\nuncheckpointed line\nmetric payload pressure line\n",
    )
    .unwrap();
    insert_session_event_flood(
        &metrics_db_path,
        file_mtime_secs(&file_path),
        1,
        16 * 1024 * 1024,
    );
    #[cfg(target_os = "linux")]
    let metric_baseline_hwm = daemon_hwm_kib(&repo);
    repo.stage_all_and_commit("Commit under metric payload pressure")
        .unwrap();
    file.assert_committed_lines(lines![
        "base".unattributed_human(),
        "uncheckpointed line".unattributed_human(),
        "metric payload pressure line".unattributed_human(),
    ]);
    #[cfg(target_os = "linux")]
    {
        let growth_kib = daemon_hwm_kib(&repo).saturating_sub(metric_baseline_hwm);
        eprintln!("oversized metric recovery candidate HWM growth: {growth_kib} KiB");
        assert!(
            growth_kib < 32 * 1024,
            "oversized metric recovery candidate grew daemon HWM by {growth_kib} KiB"
        );
    }

    clear_metrics(&metrics_db_path);
    fs::write(
        &file_path,
        "base\nuncheckpointed line\nmetric payload pressure line\nmetric cached id pressure line\n",
    )
    .unwrap();
    insert_session_event_flood(&metrics_db_path, file_mtime_secs(&file_path), 1, 0);
    replace_cached_metric_session_id(&metrics_db_path, &"s".repeat(32 * 1024 * 1024));
    #[cfg(target_os = "linux")]
    let metric_cached_id_baseline_hwm = daemon_hwm_kib(&repo);
    repo.stage_all_and_commit("Commit under metric cached identifier pressure")
        .unwrap();
    file.assert_committed_lines(lines![
        "base".unattributed_human(),
        "uncheckpointed line".unattributed_human(),
        "metric payload pressure line".unattributed_human(),
        "metric cached id pressure line".unattributed_human(),
    ]);
    #[cfg(target_os = "linux")]
    {
        let growth_kib = daemon_hwm_kib(&repo).saturating_sub(metric_cached_id_baseline_hwm);
        eprintln!("oversized metric cached identifier HWM growth: {growth_kib} KiB");
        assert!(
            growth_kib < 32 * 1024,
            "oversized metric cached identifier grew daemon HWM by {growth_kib} KiB"
        );
    }

    clear_metrics(&metrics_db_path);
    fs::write(
        &file_path,
        "base\nuncheckpointed line\nmetric payload pressure line\nmetric cached id pressure line\nbash payload pressure line\n",
    )
    .unwrap();
    let repo_work_dir = repo.canonical_path().to_string_lossy().to_string();
    let mut bash_db = BashHistoryDatabase::open_at_path(&bash_db_path).unwrap();
    bash_db
        .record_start(&BashCallStart {
            original_cwd: repo_work_dir.clone(),
            repo_work_dir: Some(repo_work_dir),
            repo_discovery_error: None,
            session_id: "bash-payload-session".to_string(),
            tool_use_id: "bash-payload-tool".to_string(),
            agent_id: AgentId {
                tool: "codex".to_string(),
                id: "bash-payload-session".to_string(),
                model: "gpt-5".to_string(),
            },
            start_trace_id: "bash-payload-trace".to_string(),
            started_at_ns: file_mtime_ns(&file_path),
            command: Some("printf pressure".to_string()),
            metadata: HashMap::from([("payload".to_string(), "x".repeat(16 * 1024 * 1024))]),
        })
        .unwrap();
    drop(bash_db);
    #[cfg(target_os = "linux")]
    let bash_baseline_hwm = daemon_hwm_kib(&repo);
    repo.stage_all_and_commit("Commit under bash payload pressure")
        .unwrap();
    file.assert_committed_lines(lines![
        "base".unattributed_human(),
        "uncheckpointed line".unattributed_human(),
        "metric payload pressure line".unattributed_human(),
        "metric cached id pressure line".unattributed_human(),
        "bash payload pressure line".unattributed_human(),
    ]);
    #[cfg(target_os = "linux")]
    {
        let growth_kib = daemon_hwm_kib(&repo).saturating_sub(bash_baseline_hwm);
        eprintln!("oversized bash recovery candidate HWM growth: {growth_kib} KiB");
        assert!(
            growth_kib < 32 * 1024,
            "oversized bash recovery candidate grew daemon HWM by {growth_kib} KiB"
        );
    }

    fs::write(
        &file_path,
        "base\nuncheckpointed line\nmetric payload pressure line\nmetric cached id pressure line\nbash payload pressure line\ncheckpointed AI line\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI commit after recovery query pressure")
        .unwrap();
    file.assert_committed_lines(lines![
        "base".unattributed_human(),
        "uncheckpointed line".unattributed_human(),
        "metric payload pressure line".unattributed_human(),
        "metric cached id pressure line".unattributed_human(),
        "bash payload pressure line".unattributed_human(),
        "checkpointed AI line".ai(),
    ]);
}
