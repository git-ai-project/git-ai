use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::daemon::open_local_socket_stream_with_timeout;
use std::fs;
use std::io::Write;
use std::time::Duration;

fn branch_completion_count(repo: &TestRepo) -> usize {
    repo.daemon_completion_entries()
        .iter()
        .filter(|entry| entry.primary_command.as_deref() == Some("branch"))
        .count()
}

#[test]
fn stalled_root_bounds_ready_commands_and_recovers_attribution() {
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_MAX_SEQUENCER_READY_COMMANDS", "2")]);
    let file_path = repo.path().join("tracked.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);
    let initial_branch_completions = branch_completion_count(&repo);

    let mut trace = open_local_socket_stream_with_timeout(
        &repo.daemon_trace_socket_path(),
        Duration::from_secs(2),
    )
    .unwrap();
    writeln!(
        trace,
        "{}",
        serde_json::json!({
            "event": "start",
            "sid": "sequencer-pressure-root",
            "argv": ["git", "commit", "-m", "stalled"],
            "worktree": repo.canonical_path(),
            "time_ns": 1,
        })
    )
    .unwrap();
    trace.flush().unwrap();
    std::thread::sleep(Duration::from_millis(250));

    for index in 0..3 {
        let branch = format!("sequencer-pressure-{index}");
        repo.git_without_test_sync_for_test(&["branch", &branch], &[])
            .unwrap();
    }
    std::thread::sleep(Duration::from_millis(250));

    drop(trace);
    repo.sync_daemon_force();
    assert_eq!(
        branch_completion_count(&repo) - initial_branch_completions,
        2,
        "the excess completed command should fail closed instead of remaining retained"
    );

    fs::write(&file_path, "base\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI commit after sequencer pressure")
        .unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "ai line".ai()]);
}
