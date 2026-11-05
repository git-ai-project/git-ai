use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::daemon::DaemonConfig;
use std::fs;
use std::time::{Duration, SystemTime};

fn assert_known_human_attestation(repo: &TestRepo, file_path: &str, line: u32) {
    let commit_sha = repo.git(&["rev-parse", "HEAD"]).unwrap();
    let note = repo
        .read_authorship_note(commit_sha.trim())
        .expect("commit should have an authorship note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse authorship note");
    let human_entry = log
        .attestations
        .iter()
        .filter(|attestation| attestation.file_path == file_path)
        .flat_map(|attestation| &attestation.entries)
        .find(|entry| {
            entry.hash.starts_with("h_")
                && entry.line_ranges.iter().any(|range| range.contains(line))
        })
        .expect("line should have a known-human attestation");
    assert!(
        log.metadata.humans.contains_key(&human_entry.hash),
        "known-human attestation should resolve through metadata.humans"
    );
}

#[test]
fn test_daemon_commit_uses_immutable_commit_content_not_next_worktree_edit() {
    let repo = TestRepo::new_dedicated_daemon();
    let mut file = repo.filename("race.txt");
    let file_path = repo.path().join("race.txt");

    fs::write(&file_path, "base\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "race.txt"])
        .unwrap();
    repo.stage_all_and_commit("base").unwrap();
    file.assert_committed_lines(crate::lines!["base".human()]);

    repo.git_ai(&["checkpoint", "human", "race.txt"]).unwrap();
    fs::write(&file_path, "base\nsecond-ai\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "race.txt"]).unwrap();
    repo.git_og(&["add", "race.txt"]).unwrap();

    let trace_target = DaemonConfig::trace2_event_target_for_path(&repo.daemon_trace_socket_path());
    repo.git_og_with_env(
        &["commit", "-m", "add ai line"],
        &[
            ("GIT_TRACE2_EVENT", trace_target.as_str()),
            ("GIT_TRACE2_EVENT_NESTING", "0"),
        ],
    )
    .unwrap();

    fs::write(&file_path, "base\nnext-operation-line\n").unwrap();
    let backdated_mtime = filetime::FileTime::from_system_time(
        SystemTime::now()
            .checked_sub(Duration::from_secs(60))
            .unwrap(),
    );
    filetime::set_file_mtime(&file_path, backdated_mtime).unwrap();

    let committed_content = repo.git_og(&["show", "HEAD:race.txt"]).unwrap();
    assert_eq!(
        committed_content, "base\nsecond-ai\n",
        "precondition: HEAD contains the AI line before daemon processing catches up"
    );
    assert_eq!(
        fs::read_to_string(&file_path).unwrap(),
        "base\nnext-operation-line\n",
        "precondition: worktree has already advanced to the next operation"
    );

    let commit_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let note = repo
        .read_authorship_note(&commit_sha)
        .expect("commit should have an authorship note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse authorship note");
    let race_attestation = log
        .attestations
        .iter()
        .find(|attestation| attestation.file_path == "race.txt")
        .expect("race.txt should have attestations");
    let ai_entry_for_line_2 = race_attestation.entries.iter().any(|entry| {
        let author_id = entry.hash.split("::").next().unwrap_or(&entry.hash);
        let has_line_2 = entry.line_ranges.iter().any(|range| range.contains(2));
        has_line_2
            && (log.metadata.sessions.contains_key(author_id)
                || log.metadata.prompts.contains_key(&entry.hash))
    });
    assert!(
        ai_entry_for_line_2,
        "committed line 2 should retain AI attribution in the immutable commit note: {:?}",
        race_attestation.entries
    );
}

#[test]
fn test_checkpointed_carryover_survives_uncheckpointed_append() {
    let repo = TestRepo::new_dedicated_daemon();
    let mut file = repo.filename("test.txt");
    let file_path = repo.path().join("test.txt");

    fn content_through(last: u32) -> String {
        (1..=last)
            .map(|line| format!("line {line}\n"))
            .collect::<String>()
    }

    fs::write(&file_path, content_through(10)).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.git(&["add", "test.txt"]).unwrap();

    fs::write(&file_path, content_through(15)).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();

    fs::write(&file_path, content_through(20)).unwrap();
    repo.commit("commit staged first ten").unwrap();
    file.assert_committed_lines(
        (1..=10)
            .map(|line| format!("line {line}").ai())
            .collect::<Vec<_>>(),
    );

    repo.stage_all_and_commit("commit remaining lines").unwrap();
    let mut expected = (1..=15)
        .map(|line| format!("line {line}").ai())
        .collect::<Vec<_>>();
    expected.extend((16..=18).map(|line| format!("line {line}").ai()));
    expected.extend((19..=20).map(|line| format!("line {line}").human()));
    file.assert_lines_and_blame(expected);
}

#[test]
fn test_uncheckpointed_human_replacement_of_pending_ai_edit_is_known_human() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");
    let file_path = repo.path().join("test.txt");

    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();
    file.assert_committed_lines(crate::lines!["base".unattributed_human()]);

    // Match an agent preset: take a pre-edit snapshot, then attribute the AI edit.
    repo.git_ai(&["checkpoint", "human", "test.txt"]).unwrap();
    fs::write(&file_path, "AAAAAAAAAA\nAI line that remains\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();

    // A human replaces the first pending AI line before the working log is consumed by
    // a commit. No known-human checkpoint fires (the condition from issue #2344).
    fs::write(&file_path, "ZZZZZZZZZZ\nAI line that remains\n").unwrap();
    repo.stage_all_and_commit("human replaces pending AI edit")
        .unwrap();

    file.assert_committed_lines(crate::lines![
        "ZZZZZZZZZZ".human(),
        "AI line that remains".ai(),
    ]);
    assert_known_human_attestation(&repo, "test.txt", 1);
}

#[test]
fn test_unstaged_human_replacement_does_not_reclaim_staged_pending_ai_edit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("test.txt");

    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    repo.git_ai(&["checkpoint", "human", "test.txt"]).unwrap();
    fs::write(&file_path, "AI staged version\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.git(&["add", "test.txt"]).unwrap();

    // The human edit has not been staged, so the immutable commit tree still contains the AI
    // version. Delayed post-commit processing must not inspect or attribute this live edit.
    fs::write(&file_path, "human worktree version\n").unwrap();
    repo.commit("commit staged AI version").unwrap();

    let committed = repo.git_og(&["show", "HEAD:test.txt"]).unwrap();
    assert_eq!(committed, "AI staged version\n");
    let commit_sha = repo.git(&["rev-parse", "HEAD"]).unwrap();
    let note = repo
        .read_authorship_note(commit_sha.trim())
        .expect("commit should have an authorship note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse authorship note");
    let attestation = log
        .attestations
        .iter()
        .find(|attestation| attestation.file_path == "test.txt")
        .expect("test.txt should have attestations");
    assert!(
        attestation.entries.iter().any(|entry| {
            !entry.hash.starts_with("h_") && entry.line_ranges.iter().any(|range| range.contains(1))
        }),
        "the committed AI line must retain AI attribution: {:?}",
        attestation.entries
    );
}

#[test]
fn test_earlier_staged_ai_checkpoint_is_not_reclaimed_by_later_ai_checkpoint() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("test.txt");

    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    repo.git_ai(&["checkpoint", "human", "test.txt"]).unwrap();
    fs::write(&file_path, "first AI version\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.git(&["add", "test.txt"]).unwrap();

    fs::write(&file_path, "later AI version\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.commit("commit first AI version").unwrap();

    let commit_sha = repo.git(&["rev-parse", "HEAD"]).unwrap();
    let note = repo
        .read_authorship_note(commit_sha.trim())
        .expect("commit should have an authorship note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse authorship note");
    let attestation = log
        .attestations
        .iter()
        .find(|attestation| attestation.file_path == "test.txt")
        .expect("test.txt should have attestations");
    assert!(
        attestation.entries.iter().any(|entry| {
            !entry.hash.starts_with("h_") && entry.line_ranges.iter().any(|range| range.contains(1))
        }),
        "the earlier staged AI line must retain AI attribution: {:?}",
        attestation.entries
    );
}

#[test]
fn test_uncheckpointed_human_restore_of_known_human_checkpoint_is_known_human() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");
    let file_path = repo.path().join("test.txt");

    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    fs::write(&file_path, "human version\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();
    fs::write(&file_path, "AI version\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();

    fs::write(&file_path, "human version\n").unwrap();
    repo.stage_all_and_commit("restore human version").unwrap();

    file.assert_committed_lines(crate::lines!["human version".human()]);
    assert_known_human_attestation(&repo, "test.txt", 1);
}

#[test]
fn test_autocrlf_worktree_preserves_ai_attribution_after_commit() {
    let repo = TestRepo::new();
    repo.git_og(&["config", "core.autocrlf", "true"]).unwrap();

    let file_path = repo.path().join("s.dart");
    let mut file = repo.filename("s.dart");
    fs::write(
        &file_path,
        "class A {\n  void a() {}\n  void b() {}\n  void c() {}\n}\n",
    )
    .unwrap();
    repo.stage_all_and_commit("baseline").unwrap();
    file.assert_committed_lines(crate::lines![
        "class A {".unattributed_human(),
        "  void a() {}".unattributed_human(),
        "  void b() {}".unattributed_human(),
        "  void c() {}".unattributed_human(),
        "}".unattributed_human(),
    ]);

    fs::write(
        &file_path,
        "class A {\r\n  void a() {}\r\n  void b() {}\r\n  void c() {}\r\n  void ai1() {}\r\n  void ai2() {}\r\n  void ai3() {}\r\n}\r\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "s.dart"]).unwrap();
    repo.stage_all_and_commit("ai adds three methods").unwrap();

    file.assert_committed_lines(crate::lines![
        "class A {".unattributed_human(),
        "  void a() {}".unattributed_human(),
        "  void b() {}".unattributed_human(),
        "  void c() {}".unattributed_human(),
        "  void ai1() {}".ai(),
        "  void ai2() {}".ai(),
        "  void ai3() {}".ai(),
        "}".unattributed_human(),
    ]);

    let stats = repo.stats().unwrap();
    assert_eq!(stats.ai_additions, 3);
    assert_eq!(stats.ai_accepted, 3);
    assert_eq!(stats.unknown_additions, 0);
}
