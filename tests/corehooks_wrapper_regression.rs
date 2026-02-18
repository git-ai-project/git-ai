mod repos;

use git_ai::git::repository::find_repository_in_path;
use git_ai::git::rewrite_log::{ResetKind, RewriteLogEvent};
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use std::fs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RewriteEventCounts {
    commit: usize,
    amend: usize,
    reset: usize,
    rebase_complete: usize,
    cherry_pick_complete: usize,
}

fn rewrite_event_counts(repo: &TestRepo) -> RewriteEventCounts {
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
    let amend_events = events
        .iter()
        .filter(|event| matches!(event, RewriteLogEvent::CommitAmend { .. }))
        .count();
    let reset_events = events
        .iter()
        .filter(|event| matches!(event, RewriteLogEvent::Reset { .. }))
        .count();
    let rebase_complete_events = events
        .iter()
        .filter(|event| matches!(event, RewriteLogEvent::RebaseComplete { .. }))
        .count();
    let cherry_pick_complete_events = events
        .iter()
        .filter(|event| matches!(event, RewriteLogEvent::CherryPickComplete { .. }))
        .count();

    RewriteEventCounts {
        commit: commit_events,
        amend: amend_events,
        reset: reset_events,
        rebase_complete: rebase_complete_events,
        cherry_pick_complete: cherry_pick_complete_events,
    }
}

fn latest_reset_kind(repo: &TestRepo) -> Option<ResetKind> {
    let gitai_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("failed to open repository");
    let events = gitai_repo
        .storage
        .read_rewrite_events()
        .expect("failed to read rewrite events");

    events.into_iter().find_map(|event| match event {
        RewriteLogEvent::Reset { reset } => Some(reset.kind),
        _ => None,
    })
}

#[test]
fn test_commit_dry_run_does_not_record_rewrite_event() {
    let repo = TestRepo::new();

    let mut file = repo.filename("test.txt");
    file.set_contents(vec!["base".to_string()]);
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    let before_counts = rewrite_event_counts(&repo);

    file.set_contents(vec!["base".human(), "ai line".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.git(&["add", "test.txt"]).expect("add should succeed");
    repo.git(&["commit", "--dry-run"])
        .expect("commit --dry-run should succeed");

    let after_counts = rewrite_event_counts(&repo);
    assert_eq!(
        after_counts.commit, before_counts.commit,
        "dry-run commit must not append rewrite events"
    );
}

#[test]
fn test_commit_rewrite_event_recorded_once() {
    let repo = TestRepo::new();

    let mut file = repo.filename("test.txt");
    file.set_contents(vec!["base".to_string()]);
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    let before_counts = rewrite_event_counts(&repo);

    file.set_contents(vec!["base".human(), "ai line".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.stage_all_and_commit("ai commit")
        .expect("ai commit should succeed");

    let after_counts = rewrite_event_counts(&repo);
    assert_eq!(
        after_counts.commit,
        before_counts.commit + 1,
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

    let before_counts = rewrite_event_counts(&repo);
    repo.git(&["reset", "--mixed", &first_commit.commit_sha])
        .expect("reset should succeed");

    let after_counts = rewrite_event_counts(&repo);
    assert_eq!(
        after_counts.reset,
        before_counts.reset + 1,
        "expected exactly one reset rewrite event for a single reset operation",
    );
}

#[test]
fn test_reset_hard_with_untracked_files_records_hard_mode() {
    let repo = TestRepo::new();

    let mut file = repo.filename("tracked.txt");
    file.set_contents(vec!["line 1".to_string()]);
    let first_commit = repo
        .stage_all_and_commit("first commit")
        .expect("first commit should succeed");

    file.set_contents(vec!["line 1".human(), "line 2 ai".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.stage_all_and_commit("second commit")
        .expect("second commit should succeed");

    fs::write(repo.path().join("scratch.tmp"), "left alone\n").expect("write untracked file");
    let before_counts = rewrite_event_counts(&repo);

    repo.git(&["reset", "--hard", &first_commit.commit_sha])
        .expect("reset --hard should succeed");

    let after_counts = rewrite_event_counts(&repo);
    assert_eq!(
        after_counts.reset,
        before_counts.reset + 1,
        "expected exactly one reset rewrite event for reset --hard",
    );
    assert_eq!(
        latest_reset_kind(&repo),
        Some(ResetKind::Hard),
        "untracked files must not cause reset --hard to be recorded as mixed",
    );
    assert!(
        repo.read_file("scratch.tmp").is_some(),
        "untracked file should remain after reset --hard",
    );
}

#[test]
fn test_commit_amend_rewrite_event_recorded_once() {
    let repo = TestRepo::new();

    let mut file = repo.filename("test.txt");
    file.set_contents(vec!["base".to_string()]);
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    let before_counts = rewrite_event_counts(&repo);

    file.set_contents(vec!["base".human(), "amended ai line".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.git(&["add", "test.txt"]).expect("add should succeed");
    repo.git(&["commit", "--amend", "-m", "amended base commit"])
        .expect("amend commit should succeed");

    let after_counts = rewrite_event_counts(&repo);
    assert_eq!(
        after_counts.amend,
        before_counts.amend + 1,
        "expected exactly one amend rewrite event for commit --amend",
    );
}

#[test]
fn test_rebase_complete_rewrite_event_recorded_once() {
    let repo = TestRepo::new();
    let default_branch = repo.current_branch();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(vec!["base".to_string()]);
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout feature branch");
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(vec!["feature ai line".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    repo.stage_all_and_commit("feature commit")
        .expect("feature commit should succeed");

    repo.git(&["checkout", &default_branch])
        .expect("checkout default branch");
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(vec!["main human line".human()]);
    repo.stage_all_and_commit("main update")
        .expect("main update should succeed");

    repo.git(&["checkout", "feature"])
        .expect("checkout feature again");
    let before_counts = rewrite_event_counts(&repo);
    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed");
    let after_counts = rewrite_event_counts(&repo);

    assert_eq!(
        after_counts.rebase_complete,
        before_counts.rebase_complete + 1,
        "expected exactly one rebase completion event for a single rebase",
    );
    feature_file.assert_lines_and_blame(vec!["feature ai line".ai()]);
}

#[test]
fn test_cherry_pick_complete_rewrite_event_recorded_once() {
    let repo = TestRepo::new();
    let default_branch = repo.current_branch();

    let mut file = repo.filename("cherry.txt");
    file.set_contents(vec!["base".to_string()]);
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    repo.git(&["checkout", "-b", "source"])
        .expect("checkout source branch");
    file.set_contents(vec!["base".human(), "source ai line".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");
    let source_commit = repo
        .stage_all_and_commit("source commit")
        .expect("source commit should succeed")
        .commit_sha;

    repo.git(&["checkout", &default_branch])
        .expect("checkout default branch");
    let before_counts = rewrite_event_counts(&repo);
    repo.git(&["cherry-pick", &source_commit])
        .expect("cherry-pick should succeed");
    let after_counts = rewrite_event_counts(&repo);

    assert_eq!(
        after_counts.cherry_pick_complete,
        before_counts.cherry_pick_complete + 1,
        "expected exactly one cherry-pick completion event for a single cherry-pick",
    );
    file.assert_lines_and_blame(vec!["base".human(), "source ai line".ai()]);
}
