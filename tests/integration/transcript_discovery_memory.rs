use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::types::MetricEventId;
use git_ai::metrics::{PosEncoded, SessionEventValues};
use serde_json::json;
use std::fs;
use std::time::{Duration, Instant, SystemTime};

const DISCOVERY_LIMIT_PLUS_ONE: usize = 4_097;

fn wait_for_live_event(metrics_db_path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(db) = MetricsDatabase::open_at_path(metrics_db_path)
            && db
                .get_metric_history(0, None, &[MetricEventId::SessionEvent as u16])
                .unwrap_or_default()
                .iter()
                .any(|record| {
                    SessionEventValues::from_sparse(&record.event.values)
                        .raw_json
                        .get("uuid")
                        .and_then(|value| value.as_str())
                        == Some("live-discovery-event")
                })
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "newest transcript was not processed after bounded discovery"
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
fn bounded_transcript_discovery_keeps_newest_session_and_daemon_usable() {
    let fixture = tempfile::tempdir().unwrap();
    let claude_config = fixture.path().join("claude");
    let projects = claude_config.join("projects/test-project");
    fs::create_dir_all(&projects).unwrap();

    let old_mtime = filetime::FileTime::from_system_time(
        SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60),
    );
    for index in 0..DISCOVERY_LIMIT_PLUS_ONE {
        let path = projects.join(format!("historical-{index:05}.jsonl"));
        fs::write(&path, "{}\n").unwrap();
        filetime::set_file_mtime(path, old_mtime).unwrap();
    }

    let live_transcript = projects.join("live-discovery-session.jsonl");
    fs::write(
        &live_transcript,
        format!(
            "{}\n",
            json!({
                "uuid": "live-discovery-event",
                "timestamp": "2026-07-10T00:00:00Z",
                "cwd": fixture.path(),
                "message": { "content": "bounded discovery" },
            })
        ),
    )
    .unwrap();

    let metrics_db_path = fixture.path().join("metrics.db");
    let claude_config_str = claude_config.to_string_lossy().to_string();
    let metrics_db_path_str = metrics_db_path.to_string_lossy().to_string();
    let repo = TestRepo::new_with_daemon_env(&[
        ("CLAUDE_CONFIG_DIR", claude_config_str.as_str()),
        ("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path_str.as_str()),
    ]);

    wait_for_live_event(&metrics_db_path);

    #[cfg(target_os = "linux")]
    assert!(
        daemon_hwm_kib(&repo) < 128 * 1024,
        "bounded transcript discovery exceeded 128 MiB daemon HWM"
    );

    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after discovery pressure")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
