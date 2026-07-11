use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

#[test]
fn oversized_github_event_is_rejected_and_checkpoint_recovers() {
    let repo = TestRepo::new_dedicated_daemon();
    let tracked_path = repo.path().join("tracked.txt");
    fs::write(&tracked_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut tracked = repo.filename("tracked.txt");
    tracked.assert_committed_lines(crate::lines!["base".unattributed_human()]);

    let event_path = repo.path().join("oversized-event.json");
    let event = fs::File::create(&event_path).unwrap();
    event.set_len(16 * 1024 * 1024 + 1).unwrap();
    let event_path_string = event_path.to_string_lossy().to_string();
    let error = repo
        .git_ai_with_env(
            &["ci", "github", "run"],
            &[
                ("GITHUB_EVENT_NAME", "pull_request"),
                ("GITHUB_EVENT_PATH", event_path_string.as_str()),
            ],
        )
        .expect_err("oversized event must be rejected");
    assert!(error.contains("byte limit"), "unexpected error: {error}");
    fs::remove_file(event_path).unwrap();

    fs::write(&tracked_path, "base\nAI recovery\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI recovery after auxiliary input pressure")
        .unwrap();
    tracked.assert_committed_lines(crate::lines![
        "base".unattributed_human(),
        "AI recovery".ai(),
    ]);
}
