use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::git::refs::notes_add_blob_batch;
use git_ai::git::repository::find_repository_in_path;
use std::fs;

const SOURCE_COMMIT_COUNT: usize = 17;
const SHARED_NOTE_BYTES: usize = 4 * 1_024 * 1_024;

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

fn assert_source_files(repo: &TestRepo, count: usize, expect_ai: bool) {
    for index in 0..count {
        let line = format!("source {index}");
        let expected = if expect_ai {
            line.ai()
        } else {
            line.unattributed_human()
        };
        let mut file = repo.filename(&format!("source-{index}.txt"));
        file.assert_committed_lines(crate::lines![expected]);
    }
}

#[test]
fn shared_note_blob_materialization_keeps_daemon_bounded_and_recovers() {
    let repo = TestRepo::new_dedicated_daemon();
    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    repo.stage_all_and_commit("base commit").unwrap();
    assert_base(&repo);

    let base_branch = repo.current_branch();
    repo.git(&["switch", "-c", "large-note-sources"]).unwrap();
    let mut source_shas = Vec::new();
    for index in 0..SOURCE_COMMIT_COUNT {
        let path = format!("source-{index}.txt");
        fs::write(repo.path().join(&path), format!("source {index}\n")).unwrap();
        repo.git_ai(&["checkpoint", "mock_ai", &path]).unwrap();
        repo.stage_all_and_commit(&format!("source commit {index}"))
            .unwrap();
        source_shas.push(
            repo.git_og(&["rev-parse", "HEAD"])
                .unwrap()
                .trim()
                .to_string(),
        );
        assert_base(&repo);
        assert_source_files(&repo, index + 1, true);
    }

    let note_file = tempfile::NamedTempFile::new().unwrap();
    fs::write(note_file.path(), vec![b'x'; SHARED_NOTE_BYTES]).unwrap();
    let note_path = note_file.path().to_string_lossy();
    let note_blob_oid = repo
        .git_og(&["hash-object", "-w", note_path.as_ref()])
        .unwrap()
        .trim()
        .to_string();
    let note_entries = source_shas
        .iter()
        .map(|sha| (sha.clone(), note_blob_oid.clone()))
        .collect::<Vec<_>>();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    notes_add_blob_batch(&gitai_repo, &note_entries).unwrap();

    repo.git(&["switch", &base_branch]).unwrap();
    assert_base(&repo);

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);
    #[cfg(target_os = "linux")]
    let baseline_threads = daemon_thread_count(&repo);

    let mut cherry_pick_args = vec!["cherry-pick"];
    cherry_pick_args.extend(source_shas.iter().map(String::as_str));
    repo.git_without_test_sync_for_test(&cherry_pick_args, &[])
        .unwrap();
    repo.sync_daemon();

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        eprintln!("shared note materialization HWM growth: {hwm_growth_kib} KiB");
        assert!(
            hwm_growth_kib < 48 * 1_024,
            "shared note materialization grew daemon HWM by {hwm_growth_kib} KiB"
        );
        let materialized_threads = daemon_thread_count(&repo);
        eprintln!(
            "shared note materialization threads: baseline={baseline_threads}, materialized={materialized_threads}"
        );
        assert!(
            materialized_threads <= baseline_threads + 2,
            "note materialization must not retain worker threads: baseline={baseline_threads}, materialized={materialized_threads}"
        );
    }
    assert_base(&repo);
    assert_source_files(&repo, SOURCE_COMMIT_COUNT, false);

    fs::write(repo.path().join("recovery.txt"), "AI recovery\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "recovery.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI commit after note pressure")
        .unwrap();
    assert_base(&repo);
    assert_source_files(&repo, SOURCE_COMMIT_COUNT, false);
    let mut recovery = repo.filename("recovery.txt");
    recovery.assert_committed_lines(crate::lines!["AI recovery".ai()]);
}
