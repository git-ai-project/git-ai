use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::attribution_tracker::LineAttribution;
use git_ai::authorship::working_log::{AgentId, Checkpoint, CheckpointKind, WorkingLogEntry};
use std::fs;

const PRESSURE_FILE_COUNT: usize = 17;
const SHARED_BLOB_BYTES: usize = 4 * 1_024 * 1_024;

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

fn assert_base(repo: &TestRepo) {
    let mut file = repo.filename("base.txt");
    file.assert_committed_lines(crate::lines!["base".unattributed_human()]);
}

fn assert_pressure_files(repo: &TestRepo) {
    for index in 0..PRESSURE_FILE_COUNT {
        let mut file = repo.filename(&format!("pressure-{index}.txt"));
        file.assert_committed_lines(crate::lines![
            format!("pressure {index}").unattributed_human()
        ]);
    }
}

#[test]
fn shared_working_log_blob_materialization_keeps_daemon_bounded_and_recovers() {
    let repo = TestRepo::new_dedicated_daemon();
    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    repo.stage_all_and_commit("base commit").unwrap();
    assert_base(&repo);

    let working_log = repo.current_working_logs();
    let shared_blob = "x".repeat(SHARED_BLOB_BYTES);
    let blob_sha = working_log.persist_file_version(&shared_blob).unwrap();
    let mut entries = Vec::new();
    for index in 0..PRESSURE_FILE_COUNT {
        let path = format!("pressure-{index}.txt");
        fs::write(repo.path().join(&path), format!("pressure {index}\n")).unwrap();
        entries.push(WorkingLogEntry::new(
            path,
            blob_sha.clone(),
            Vec::new(),
            vec![LineAttribution::new(
                1,
                1,
                "working-log-pressure-ai".to_string(),
                None,
            )],
        ));
    }
    let mut checkpoint = Checkpoint::new(
        CheckpointKind::AiAgent,
        String::new(),
        "mock_ai".to_string(),
        entries,
    );
    checkpoint.agent_id = Some(AgentId {
        tool: "mock_ai".to_string(),
        id: "working-log-pressure".to_string(),
        model: "test".to_string(),
    });
    working_log.write_all_checkpoints(&[checkpoint]).unwrap();
    drop(shared_blob);

    repo.git_og(&["add", "-A"]).unwrap();
    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);
    #[cfg(target_os = "linux")]
    let baseline_threads = daemon_thread_count(&repo);

    repo.git_without_test_sync_for_test(&["commit", "-m", "working log pressure"], &[])
        .unwrap();
    repo.sync_daemon();

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        eprintln!("shared working-log blob HWM growth: {hwm_growth_kib} KiB");
        assert!(
            hwm_growth_kib < 48 * 1_024,
            "shared working-log blob grew daemon HWM by {hwm_growth_kib} KiB"
        );
        let reconstructed_threads = daemon_thread_count(&repo);
        assert!(
            reconstructed_threads <= baseline_threads + 2,
            "working-log reconstruction must not retain worker threads: baseline={baseline_threads}, reconstructed={reconstructed_threads}"
        );
    }
    assert_base(&repo);
    assert_pressure_files(&repo);

    fs::write(repo.path().join("recovery.txt"), "AI recovery\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "recovery.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI commit after working-log pressure")
        .unwrap();
    assert_base(&repo);
    assert_pressure_files(&repo);
    let mut recovery = repo.filename("recovery.txt");
    recovery.assert_committed_lines(crate::lines!["AI recovery".ai()]);
}

#[test]
fn repeated_checkpoint_file_replacement_uses_net_materialized_bytes() {
    let repo = TestRepo::new_dedicated_daemon();
    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    repo.stage_all_and_commit("base commit").unwrap();
    assert_base(&repo);

    let large_line = "x".repeat(2 * 1_024 * 1_024);
    let pressure_path = repo.path().join("pressure.txt");
    let mut contents = format!("{large_line}\n");
    let mut expected = vec![large_line.ai()];
    for index in 0..9 {
        if index > 0 {
            let line = format!("AI checkpoint {index}");
            contents.push_str(&line);
            contents.push('\n');
            expected.push(line.ai());
        }
        fs::write(&pressure_path, &contents).unwrap();
        repo.git_ai(&["checkpoint", "mock_ai", "pressure.txt"])
            .unwrap();
    }

    repo.stage_all_and_commit("repeated file checkpoints")
        .unwrap();
    assert_base(&repo);
    let mut pressure = repo.filename("pressure.txt");
    pressure.assert_committed_lines(expected);
}

#[test]
fn missing_checkpoint_blob_degrades_without_losing_committed_attribution() {
    let repo = TestRepo::new_dedicated_daemon();
    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    repo.stage_all_and_commit("base commit").unwrap();
    assert_base(&repo);

    fs::write(repo.path().join("committed.txt"), "AI survives\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "committed.txt"])
        .unwrap();

    let working_log = repo.current_working_logs();
    let mut checkpoints = working_log.read_all_checkpoints().unwrap();
    let ai_checkpoint = checkpoints
        .iter_mut()
        .find(|checkpoint| checkpoint.kind == CheckpointKind::AiAgent)
        .expect("AI checkpoint should exist");
    let committed_entry = ai_checkpoint
        .entries
        .iter()
        .find(|entry| entry.file == "committed.txt")
        .expect("committed file checkpoint entry should exist")
        .clone();
    let missing_blob_sha = working_log
        .persist_file_version("missing AI content\n")
        .unwrap();
    fs::remove_file(working_log.dir.join("blobs").join(&missing_blob_sha)).unwrap();
    ai_checkpoint.entries.push(WorkingLogEntry::new(
        "missing.txt".to_string(),
        missing_blob_sha,
        committed_entry.attributions,
        committed_entry.line_attributions,
    ));
    working_log.write_all_checkpoints(&checkpoints).unwrap();

    repo.stage_all_and_commit("commit with missing checkpoint blob")
        .unwrap();
    assert_base(&repo);
    let mut committed = repo.filename("committed.txt");
    committed.assert_committed_lines(crate::lines!["AI survives".ai()]);
}
