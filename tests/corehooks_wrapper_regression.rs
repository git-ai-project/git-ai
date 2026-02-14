mod repos;

use git_ai::git::repository::find_repository_in_path;
use git_ai::git::rewrite_log::RewriteLogEvent;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

fn rewrite_event_counts(repo: &TestRepo) -> (usize, usize) {
    let gitai_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("failed to open repository");
    let events = gitai_repo
        .storage
        .read_rewrite_events()
        .expect("failed to read rewrite events");

    let commit_events = events
        .iter()
        .filter(|event| matches!(event, RewriteLogEvent::Commit { .. }))
        .count();
    let reset_events = events
        .iter()
        .filter(|event| matches!(event, RewriteLogEvent::Reset { .. }))
        .count();
    (commit_events, reset_events)
}

#[test]
fn test_commit_rewrite_event_recorded_once() {
    let repo = TestRepo::new();

    let mut file = repo.filename("test.txt");
    file.set_contents(vec!["base".to_string()]);
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    let (before_commit_events, _) = rewrite_event_counts(&repo);

    file.set_contents(vec!["base".human(), "ai line".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.stage_all_and_commit("ai commit")
        .expect("ai commit should succeed");

    let (after_commit_events, _) = rewrite_event_counts(&repo);
    assert_eq!(
        after_commit_events,
        before_commit_events + 1,
        "expected exactly one commit rewrite event for a single commit",
    );
}

#[test]
fn test_reset_rewrite_event_recorded_once() {
    let repo = TestRepo::new();

    let mut file = repo.filename("test.txt");
    file.set_contents(vec!["line 1".to_string()]);
    let first_commit = repo
        .stage_all_and_commit("first commit")
        .expect("first commit should succeed");

    file.set_contents(vec!["line 1".human(), "ai line".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.stage_all_and_commit("second commit")
        .expect("second commit should succeed");

    let (_, before_reset_events) = rewrite_event_counts(&repo);
    repo.git(&["reset", "--mixed", &first_commit.commit_sha])
        .expect("reset should succeed");

    let (_, after_reset_events) = rewrite_event_counts(&repo);
    assert_eq!(
        after_reset_events,
        before_reset_events + 1,
        "expected exactly one reset rewrite event for a single reset operation",
    );
}
