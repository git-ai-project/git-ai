#[macro_use]
mod repos;

use crate::repos::test_repo::TestRepo;
use git_ai::git::repository;
use git_ai::git::rewrite_log::RewriteLogEvent;
use serial_test::serial;
use std::time::{Duration, Instant};

#[cfg(windows)]
const ASYNC_EVENT_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
#[cfg(not(windows))]
const ASYNC_EVENT_WAIT_TIMEOUT: Duration = Duration::from_secs(20);

fn wait_for_rewrite_events(repo: &TestRepo, expected_min_events: usize) -> Vec<RewriteLogEvent> {
    let deadline = Instant::now() + ASYNC_EVENT_WAIT_TIMEOUT;

    loop {
        let gitai_repo =
            repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
        let events = gitai_repo.storage.read_rewrite_events().unwrap_or_default();
        if events.len() >= expected_min_events {
            return events;
        }

        assert!(
            Instant::now() < deadline,
            "Timed out waiting for rewrite events; currently have {} event(s)",
            events.len()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_rewrite_event<F>(repo: &TestRepo, predicate: F)
where
    F: Fn(&RewriteLogEvent) -> bool,
{
    let deadline = Instant::now() + ASYNC_EVENT_WAIT_TIMEOUT;

    loop {
        let gitai_repo =
            repository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
        let events = gitai_repo.storage.read_rewrite_events().unwrap_or_default();
        if events.iter().any(&predicate) {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "Timed out waiting for matching rewrite event"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

async_feature_test_wrappers! {
    fn commit_handoff_uses_async_worker_and_preserves_order() {
        let repo = TestRepo::new();

        repo.filename("file.txt").set_contents(vec!["line 1"]).stage();
        let first_output = repo
            .git(&["commit", "-m", "first async commit"])
            .expect("first commit should succeed");

        repo.filename("file.txt")
            .set_contents(vec!["line 1", "line 2"])
            .stage();
        let second_output = repo
            .git(&["commit", "-m", "second async commit"])
            .expect("second commit should succeed");

        assert!(
            first_output.contains("processed async"),
            "first commit output should include async handoff log, output:\n{}",
            first_output
        );
        assert!(
            second_output.contains("processed async"),
            "second commit output should include async handoff log, output:\n{}",
            second_output
        );
        let events = wait_for_rewrite_events(&repo, 2);
        let commit_events: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                RewriteLogEvent::Commit { commit } => Some(commit.commit_sha.clone()),
                _ => None,
            })
            .collect();

        assert!(
            commit_events.len() >= 2,
            "expected at least two commit events, got {}",
            commit_events.len()
        );

        let head = repo
            .git(&["rev-parse", "HEAD"])
            .expect("resolve HEAD")
            .trim()
            .to_string();
        let head_parent = repo
            .git(&["rev-parse", "HEAD~1"])
            .expect("resolve HEAD~1")
            .trim()
            .to_string();

        // Rewrite log is newest-first.
        assert_eq!(commit_events[0], head);
        assert_eq!(commit_events[1], head_parent);
    }
}

async_feature_test_wrappers! {
    fn rebase_complete_event_is_processed_async() {
        let repo = TestRepo::new();

        repo.filename("base.txt").set_contents(vec!["base"]).stage();
        repo.git(&["commit", "-m", "base"]).unwrap();

        repo.git(&["checkout", "-b", "feature"]).unwrap();
        repo.filename("feature.txt").set_contents(vec!["feature"]).stage();
        repo.git(&["commit", "-m", "feature"]).unwrap();

        repo.git(&["checkout", "main"]).unwrap();
        repo.filename("main.txt").set_contents(vec!["main"]).stage();
        repo.git(&["commit", "-m", "main"]).unwrap();

        repo.git(&["checkout", "feature"]).unwrap();
        let rebase_output = repo.git(&["rebase", "main"]).unwrap();

        assert!(
            rebase_output.contains("processed async"),
            "rebase output should include async handoff log, output:\n{}",
            rebase_output
        );

        wait_for_rewrite_event(&repo, |event| {
            matches!(event, RewriteLogEvent::RebaseComplete { .. })
        });
    }
}

async_feature_test_wrappers! {
    fn merge_squash_event_is_processed_async() {
        let repo = TestRepo::new();

        repo.filename("base.txt").set_contents(vec!["base"]).stage();
        repo.git(&["commit", "-m", "base"]).unwrap();

        repo.git(&["checkout", "-b", "feature"]).unwrap();
        repo.filename("feature.txt").set_contents(vec!["feature"]).stage();
        repo.git(&["commit", "-m", "feature"]).unwrap();

        repo.git(&["checkout", "main"]).unwrap();
        let merge_output = repo.git(&["merge", "--squash", "feature"]).unwrap();

        assert!(
            merge_output.contains("processed async"),
            "merge --squash output should include async handoff log, output:\n{}",
            merge_output
        );

        wait_for_rewrite_event(&repo, |event| {
            matches!(event, RewriteLogEvent::MergeSquash { .. })
        });
    }
}

#[test]
#[serial]
fn async_feature_flag_disabled_keeps_sync_path() {
    // SAFETY: this test does not rely on global feature env state from other tests and only
    // sets the variable for this process scope.
    unsafe {
        std::env::remove_var("GIT_AI_ASYNC_REWRITE_HOOKS");
    }

    let repo = TestRepo::new();
    repo.filename("sync.txt").set_contents(vec!["sync"]).stage();
    let output = repo.git(&["commit", "-m", "sync commit"]).unwrap();

    assert!(
        !output.contains("processed async"),
        "sync path should not emit async handoff debug logs, output:\n{}",
        output
    );

    let events = wait_for_rewrite_events(&repo, 1);
    let has_commit = events
        .iter()
        .any(|event| matches!(event, RewriteLogEvent::Commit { .. }));
    assert!(has_commit, "commit should still be recorded synchronously");
}
