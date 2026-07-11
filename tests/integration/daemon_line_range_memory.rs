use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log::LineRange;
use git_ai::authorship::authorship_log_serialization::{
    AttestationEntry, AuthorshipLog, FileAttestation,
};
use std::fs;

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
fn oversized_compact_authorship_range_keeps_daemon_bounded_and_recovers() {
    let repo = TestRepo::new_dedicated_daemon();
    let tracked_path = repo.path().join("tracked.txt");
    fs::write(&tracked_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut tracked = repo.filename("tracked.txt");
    tracked.assert_committed_lines(crate::lines!["base".unattributed_human()]);

    fs::write(&tracked_path, "base\nsource line\n").unwrap();
    repo.stage_all_and_commit("Source commit").unwrap();
    tracked.assert_committed_lines(crate::lines![
        "base".unattributed_human(),
        "source line".unattributed_human(),
    ]);
    let source_sha = repo.git_og(&["rev-parse", "HEAD"]).unwrap();
    let source_sha = source_sha.trim();

    let mut log = AuthorshipLog::new();
    let mut file = FileAttestation::new("tracked.txt".to_string());
    file.add_entry(AttestationEntry::new(
        "malicious-range".to_string(),
        vec![LineRange::Single(1)],
    ));
    log.attestations.push(file);
    let compact_note = log
        .serialize_to_string()
        .unwrap()
        .replace("malicious-range 1", "malicious-range 1-10000000");
    repo.git_og(&[
        "notes",
        "--ref=ai",
        "add",
        "-f",
        "-m",
        &compact_note,
        source_sha,
    ])
    .unwrap();

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);
    repo.git(&["revert", "--no-edit", source_sha]).unwrap();

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        eprintln!("compact authorship range HWM growth: {hwm_growth_kib} KiB");
        assert!(
            hwm_growth_kib < 24 * 1024,
            "compact authorship range grew daemon HWM by {hwm_growth_kib} KiB"
        );
    }
    tracked.assert_committed_lines(crate::lines!["base".unattributed_human()]);

    fs::write(&tracked_path, "base\nAI recovery\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI recovery after range pressure")
        .unwrap();
    tracked.assert_committed_lines(crate::lines![
        "base".unattributed_human(),
        "AI recovery".ai(),
    ]);
}
