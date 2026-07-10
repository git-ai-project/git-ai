use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::daemon::control_api::{
    CasSyncPayload, ControlRequest, ControlResponse, TelemetryEnvelope,
};
use git_ai::daemon::{DaemonClientStream, open_local_socket_stream_with_timeout};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

struct ControlConnection {
    stream: BufReader<DaemonClientStream>,
}

impl ControlConnection {
    fn connect(repo: &TestRepo) -> Self {
        let stream = open_local_socket_stream_with_timeout(
            &repo.daemon_control_socket_path(),
            Duration::from_secs(2),
        )
        .expect("test daemon control socket should connect");
        Self {
            stream: BufReader::new(stream),
        }
    }

    fn send(&mut self, request: &ControlRequest) {
        serde_json::to_writer(self.stream.get_mut(), request).unwrap();
        self.stream.get_mut().write_all(b"\n").unwrap();
        self.stream.get_mut().flush().unwrap();

        let mut response = String::new();
        self.stream.read_line(&mut response).unwrap();
        let response: ControlResponse = serde_json::from_str(response.trim()).unwrap();
        assert!(response.ok, "control request failed: {:?}", response.error);
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
fn telemetry_burst_keeps_daemon_memory_bounded_and_attribution_working() {
    let repo = TestRepo::new_dedicated_daemon();
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);

    let large_payload = "x".repeat(1024 * 1024);
    let mut control = ControlConnection::connect(&repo);
    for index in 0..16 {
        control.send(&ControlRequest::SubmitTelemetry {
            envelopes: vec![TelemetryEnvelope::Message {
                timestamp: "2026-07-10T00:00:00Z".to_string(),
                message: large_payload.clone(),
                level: "warning".to_string(),
                context: None,
            }],
        });
        control.send(&ControlRequest::SubmitCas {
            records: vec![CasSyncPayload {
                hash: format!("hash-{index}"),
                data: large_payload.clone(),
                metadata: None,
            }],
        });
    }
    control.send(&ControlRequest::Ping);

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        assert!(
            hwm_growth_kib < 24 * 1024,
            "telemetry burst grew daemon HWM by {hwm_growth_kib} KiB"
        );
    }

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after telemetry burst")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
