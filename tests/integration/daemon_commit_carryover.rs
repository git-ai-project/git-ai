use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::daemon::DaemonConfig;
use std::fs;
use std::time::{Duration, SystemTime};

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

#[test]
fn test_commit_time_import_reordering_preserves_ai_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("consumer.js");
    let mut file = repo.filename("consumer.js");
    fs::write(
        &file_path,
        "const base = 0;\nimport m1 from 'm1';\nimport n1 from 'n1';\nimport o1 from 'o1';\nimport p1 from 'p1';\n",
    )
    .unwrap();
    repo.stage_all_and_commit("baseline").unwrap();
    file.assert_committed_lines(crate::lines![
        "const base = 0;".unattributed_human(),
        "import m1 from 'm1';".unattributed_human(),
        "import n1 from 'n1';".unattributed_human(),
        "import o1 from 'o1';".unattributed_human(),
        "import p1 from 'p1';".unattributed_human(),
    ]);

    repo.git_ai(&["checkpoint", "human", "consumer.js"])
        .unwrap();
    fs::write(
        &file_path,
        "const base = 0;\nimport m1 from 'm1';\nimport n1 from 'n1';\nimport o1 from 'o1';\nimport p1 from 'p1';\nimport d1 from 'd1';\nimport b1 from 'b1';\nimport a1 from 'a1';\nimport c1 from 'c1';\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "consumer.js"])
        .unwrap();

    let status: serde_json::Value =
        serde_json::from_str(&repo.git_ai(&["status", "--json"]).unwrap()).unwrap();
    assert_eq!(status["stats"]["ai_additions"], 4);
    assert_eq!(status["stats"]["unknown_additions"], 0);

    // Simulate an import sorter running after the AI checkpoint but before the
    // index snapshot is committed.
    fs::write(
        &file_path,
        "const base = 0;\nimport a1 from 'a1';\nimport b1 from 'b1';\nimport c1 from 'c1';\nimport d1 from 'd1';\nimport m1 from 'm1';\nimport n1 from 'n1';\nimport o1 from 'o1';\nimport p1 from 'p1';\n",
    )
    .unwrap();
    repo.stage_all_and_commit("AI imports sorted before commit")
        .unwrap();

    file.assert_committed_lines(crate::lines![
        "const base = 0;".unattributed_human(),
        "import a1 from 'a1';".ai(),
        "import b1 from 'b1';".ai(),
        "import c1 from 'c1';".ai(),
        "import d1 from 'd1';".ai(),
        "import m1 from 'm1';".unattributed_human(),
        "import n1 from 'n1';".unattributed_human(),
        "import o1 from 'o1';".unattributed_human(),
        "import p1 from 'p1';".unattributed_human(),
    ]);

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 4);
    assert_eq!(stats.ai_additions, 4);
    assert_eq!(stats.unknown_additions, 0);

    let initial = repo.current_working_logs().read_initial_attributions();
    assert!(
        initial.files.is_empty(),
        "clean commit left phantom INITIAL attribution: {initial:#?}"
    );
}
