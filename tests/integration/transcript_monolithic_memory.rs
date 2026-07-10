use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::types::MetricEventId;
use git_ai::metrics::{PosEncoded, SessionEventValues};
use serde_json::json;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

const OVERSIZED_PAYLOAD_BYTES: usize = 64 * 1_024 * 1_024;

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

#[cfg(target_os = "linux")]
fn daemon_thread_count(repo: &TestRepo) -> u64 {
    let pid = repo.daemon_pid().expect("test repo should own a daemon");
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("Threads:")
                .and_then(|value| value.trim().parse().ok())
        })
        .expect("daemon status should include Threads")
}

fn write_oversized_continue_transcript(path: &Path) {
    let file = File::create(path).unwrap();
    let mut writer = BufWriter::new(file);
    writer
        .write_all(b"{\"history\":[{\"uuid\":\"oversized-monolithic\",\"message\":{\"content\":\"")
        .unwrap();
    let chunk = vec![b'x'; 1_024 * 1_024];
    for _ in 0..OVERSIZED_PAYLOAD_BYTES / chunk.len() {
        writer.write_all(&chunk).unwrap();
    }
    writer.write_all(b"\"}}]}").unwrap();
    writer.flush().unwrap();
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
                        .get("uuid")
                        .and_then(|value| value.as_str())
                        == Some("monolithic-recovery")
                })
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "valid transcript was not processed after oversized input"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn monolithic_transcript_pressure_keeps_daemon_bounded_and_recovers() {
    let fixture = tempfile::tempdir().unwrap();
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
    file.assert_committed_lines(crate::lines!["base".unattributed_human()]);

    let transcript_path = fixture.path().join("continue-session.json");
    write_oversized_continue_transcript(&transcript_path);
    let hook_input = |hook_event_name: &str| {
        json!({
            "cwd": repo.canonical_path().to_string_lossy(),
            "hook_event_name": hook_event_name,
            "session_id": "monolithic-memory-session",
            "tool_name": "edit",
            "tool_use_id": "monolithic-memory-tool-use",
            "transcript_path": transcript_path.to_string_lossy(),
            "tool_input": { "file_path": file_path.to_string_lossy() },
        })
        .to_string()
    };

    let pre_hook = hook_input("PreToolUse");
    repo.git_ai(&["checkpoint", "continue-cli", "--hook-input", &pre_hook])
        .unwrap();
    fs::write(&file_path, "base\nai line\n").unwrap();

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);
    #[cfg(target_os = "linux")]
    let baseline_threads = daemon_thread_count(&repo);

    let post_hook = hook_input("PostToolUse");
    repo.git_ai(&["checkpoint", "continue-cli", "--hook-input", &post_hook])
        .unwrap();
    std::thread::sleep(Duration::from_secs(3));

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        eprintln!("monolithic transcript HWM growth: {hwm_growth_kib} KiB");
        assert!(
            hwm_growth_kib < 32 * 1_024,
            "monolithic transcript grew daemon HWM by {hwm_growth_kib} KiB"
        );
        assert!(
            daemon_thread_count(&repo) <= baseline_threads + 2,
            "transcript parsing must not retain worker threads"
        );
    }

    fs::write(
        &transcript_path,
        json!({
            "history": [{
                "uuid": "monolithic-recovery",
                "message": {"content": "recovered"},
            }],
        })
        .to_string(),
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "continue-cli", "--hook-input", &post_hook])
        .unwrap();
    wait_for_recovery_event(&metrics_db_path);

    repo.stage_all_and_commit("AI edit after monolithic transcript pressure")
        .unwrap();
    file.assert_committed_lines(crate::lines!["base".unattributed_human(), "ai line".ai()]);
}
