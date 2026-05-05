/// Tests for daemon write-before-process race condition (#1079).
///
/// ## The Bug
///
/// Prior to fix in commit 272ae97b, the daemon would:
/// 1. Append the rewrite event to the rewrite log
/// 2. Process authorship (call `rewrite_authorship_if_needed`)
/// 3. If processing failed, the event was already logged
/// 4. The event would never be retried because it was marked as "processed"
///
/// This resulted in silent note loss when rebase processing failed (e.g., due to
/// corrupted working logs, missing commits, or other transient errors).
///
/// ## The Fix
///
/// The daemon now:
/// 1. Reads the current rewrite log (before appending)
/// 2. Processes authorship with the pre-append log
/// 3. Only appends the event AFTER processing succeeds
/// 4. If processing fails, the event is not logged and can be retried
///
/// ## Test Strategy
///
/// Since we can't easily simulate processing failures in integration tests without
/// complex fault injection, these tests verify:
///
/// 1. **Event ordering**: The rewrite log only contains events for successfully
///    processed rebases (notes exist for all commits in the event)
/// 2. **Idempotency**: Running the same rebase multiple times doesn't duplicate
///    events or corrupt notes
/// 3. **Daemon mode**: Specifically test daemon mode where the race was possible
use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::git::repository as GitAiRepository;
use git_ai::git::rewrite_log::RewriteLogEvent;

/// Helper to parse the rewrite log and find RebaseComplete events
fn find_rebase_complete_events(repo: &TestRepo) -> Vec<(String, String)> {
    let gitai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    let events = gitai_repo
        .storage
        .read_rewrite_events()
        .expect("read rewrite log");

    events
        .into_iter()
        .filter_map(|event| match event {
            RewriteLogEvent::RebaseComplete { rebase_complete } => Some((
                rebase_complete.original_head.clone(),
                rebase_complete.new_head.clone(),
            )),
            _ => None,
        })
        .collect()
}

/// Helper to check if a commit has an authorship note
fn has_authorship_note(repo: &TestRepo, commit_sha: &str) -> bool {
    repo.read_authorship_note(commit_sha).is_some()
}

#[test]
fn test_rebase_events_only_logged_after_successful_processing() {
    // Create a simple rebase scenario with AI-touched files
    let repo = TestRepo::new();

    // Create initial commit on main
    let mut file = repo.filename("test.rs");
    file.set_contents(lines!["fn main() {}", "    println!(\"hello\");".ai(), "}"]);
    repo.stage_all_and_commit("Initial commit")
        .expect("initial commit should succeed");
    let _initial_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("HEAD should exist")
        .trim()
        .to_string();

    // Create feature branch with 2 AI commits
    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout feature branch should succeed");
    file.set_contents(lines![
        "fn main() {",
        "    println!(\"hello\");".ai(),
        "    println!(\"world\");".ai(),
        "}"
    ]);
    repo.stage_all_and_commit("Add world")
        .expect("feature commit 1 should succeed");
    let _feature_commit1 = repo
        .git(&["rev-parse", "HEAD"])
        .expect("HEAD should exist after feature commit 1")
        .trim()
        .to_string();

    file.set_contents(lines![
        "fn main() {",
        "    println!(\"hello\");".ai(),
        "    println!(\"world\");".ai(),
        "    println!(\"!\");".ai(),
        "}"
    ]);
    repo.stage_all_and_commit("Add exclamation")
        .expect("feature commit 2 should succeed");
    let _feature_commit2 = repo
        .git(&["rev-parse", "HEAD"])
        .expect("HEAD should exist after feature commit 2")
        .trim()
        .to_string();

    // Advance main with a disjoint change
    repo.git(&["checkout", "main"])
        .expect("checkout main should succeed");
    let mut other_file = repo.filename("other.rs");
    other_file.set_contents(lines!["fn other() {}".ai()]);
    repo.stage_all_and_commit("Add other")
        .expect("main advance commit should succeed");

    // Record the rewrite log state before rebase
    let events_before = find_rebase_complete_events(&repo);

    // Rebase feature onto main
    repo.git(&["checkout", "feature"])
        .expect("checkout feature for rebase should succeed");
    repo.git(&["rebase", "main"])
        .expect("rebase should succeed without conflicts");

    let new_commit1 = repo
        .git(&["log", "--format=%H", "--skip=1", "-n", "1"])
        .expect("git log should succeed")
        .trim()
        .to_string();
    let new_commit2 = repo
        .git(&["rev-parse", "HEAD"])
        .expect("HEAD should exist after rebase")
        .trim()
        .to_string();

    // Verify both rebased commits have authorship notes
    assert!(
        has_authorship_note(&repo, &new_commit1),
        "First rebased commit should have authorship note"
    );
    assert!(
        has_authorship_note(&repo, &new_commit2),
        "Second rebased commit should have authorship note"
    );

    // Check the rewrite log for the rebase event
    let events_after = find_rebase_complete_events(&repo);

    // Should have exactly one new rebase event
    assert_eq!(
        events_after.len(),
        events_before.len() + 1,
        "Should have exactly one new rebase event"
    );

    // The new event should reference the completed rebase
    let new_event = events_after
        .last()
        .expect("events_after should not be empty");
    assert_eq!(
        new_event.1, new_commit2,
        "Event new_head should match current HEAD"
    );

    // Verify that all commits referenced in the event have notes
    // (This is the key invariant: if an event is logged, processing succeeded)
    let gitai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    let events = gitai_repo
        .storage
        .read_rewrite_events()
        .expect("read rewrite log");

    for event in events {
        if let RewriteLogEvent::RebaseComplete { rebase_complete } = event {
            // Every new commit in the event should have a note
            for commit in &rebase_complete.new_commits {
                assert!(
                    has_authorship_note(&repo, commit),
                    "Commit {} referenced in rewrite log should have authorship note",
                    commit
                );
            }
        }
    }
}

#[test]
fn test_rebase_idempotent_no_duplicate_events() {
    // Verify that if we somehow process the same rebase twice (e.g., daemon restart),
    // we don't get duplicate events or corrupted notes
    let repo = TestRepo::new();

    // Create a simple rebase scenario
    let mut file = repo.filename("test.rs");
    file.set_contents(lines!["line1".ai()]);
    repo.stage_all_and_commit("Initial")
        .expect("initial commit should succeed");

    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout feature branch should succeed");
    file.set_contents(lines!["line1".ai(), "line2".ai()]);
    repo.stage_all_and_commit("Feature")
        .expect("feature commit should succeed");

    repo.git(&["checkout", "main"])
        .expect("checkout main should succeed");
    let mut other = repo.filename("other.rs");
    other.set_contents(lines!["other".ai()]);
    repo.stage_all_and_commit("Main advance")
        .expect("main advance commit should succeed");

    // Rebase
    repo.git(&["checkout", "feature"])
        .expect("checkout feature for rebase should succeed");
    repo.git(&["rebase", "main"])
        .expect("rebase should succeed");

    let head_after_rebase = repo
        .git(&["rev-parse", "HEAD"])
        .expect("HEAD should exist after rebase")
        .trim()
        .to_string();
    let note_after_first = repo
        .read_authorship_note(&head_after_rebase)
        .expect("note exists");

    // Count rebase events
    let events = find_rebase_complete_events(&repo);
    let rebase_count_first = events.len();

    // Attempting to rebase again should be a no-op (already up to date)
    let result = repo.git(&["rebase", "main"]);
    // Either succeeds with "up to date" or exits with specific status
    if result.is_ok() {
        // Verify note is unchanged
        let note_after_second = repo
            .read_authorship_note(&head_after_rebase)
            .expect("note exists");
        assert_eq!(
            note_after_first, note_after_second,
            "Note should be unchanged after no-op rebase"
        );

        // Verify no duplicate events
        let events_after = find_rebase_complete_events(&repo);
        assert_eq!(
            events_after.len(),
            rebase_count_first,
            "No-op rebase should not add duplicate events"
        );
    }
}

#[test]
fn test_daemon_mode_rebase_events_logged_after_processing() {
    // This test specifically runs in daemon mode where the race condition
    // was possible (wrapper exits before daemon processes)
    let repo = TestRepo::new_dedicated_daemon();

    // Create rebase scenario
    let mut file = repo.filename("test.rs");
    file.set_contents(lines!["line1".ai()]);
    repo.stage_all_and_commit("Initial")
        .expect("initial commit should succeed");

    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout feature branch should succeed");
    file.set_contents(lines!["line1".ai(), "line2".ai()]);
    repo.stage_all_and_commit("Add line2")
        .expect("feature commit 1 should succeed");
    let _feature_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("HEAD should exist after feature commit 1")
        .trim()
        .to_string();

    file.set_contents(lines!["line1".ai(), "line2".ai(), "line3".ai()]);
    repo.stage_all_and_commit("Add line3")
        .expect("feature commit 2 should succeed");

    repo.git(&["checkout", "main"])
        .expect("checkout main should succeed");
    let mut other = repo.filename("other.rs");
    other.set_contents(lines!["other".ai()]);
    repo.stage_all_and_commit("Main advance")
        .expect("main advance commit should succeed");

    // Rebase in daemon mode
    repo.git(&["checkout", "feature"])
        .expect("checkout feature for rebase should succeed");
    repo.git(&["rebase", "main"])
        .expect("rebase should succeed in daemon mode");

    let new_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("HEAD should exist after rebase")
        .trim()
        .to_string();

    // In daemon mode, the wrapper may exit before the daemon finishes processing.
    // Wait for daemon to process by checking for the authorship note.
    let _note = repo
        .read_authorship_note(&new_head)
        .expect("daemon should have processed authorship note");

    // Verify the rewrite log event exists
    let events = find_rebase_complete_events(&repo);
    assert!(!events.is_empty(), "Daemon should have logged rebase event");

    // The last event should match our rebase
    let last_event = events
        .last()
        .expect("events should not be empty in daemon mode");
    assert_eq!(
        last_event.1, new_head,
        "Last rebase event should reference current HEAD"
    );

    // Verify all commits in the event have notes (key invariant)
    let gitai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    let events = gitai_repo
        .storage
        .read_rewrite_events()
        .expect("read rewrite log");

    for event in events {
        if let RewriteLogEvent::RebaseComplete { rebase_complete } = event {
            if rebase_complete.new_head == new_head {
                // This is our rebase event - verify all commits have notes
                for commit in &rebase_complete.new_commits {
                    assert!(
                        has_authorship_note(&repo, commit),
                        "Daemon should have processed note for commit {} before logging event",
                        commit
                    );
                }
            }
        }
    }
}

#[test]
fn test_rebase_event_order_matches_processing_order() {
    // Verify that events appear in the rewrite log in the order they were
    // successfully processed, not the order they were attempted
    let repo = TestRepo::new();

    // Create a scenario with multiple rebases
    let mut file = repo.filename("test.rs");
    file.set_contents(lines!["line1".ai()]);
    repo.stage_all_and_commit("Initial")
        .expect("initial commit should succeed");

    // First feature branch
    repo.git(&["checkout", "-b", "feature1"])
        .expect("checkout feature1 should succeed");
    file.set_contents(lines!["line1".ai(), "feature1".ai()]);
    repo.stage_all_and_commit("Feature 1")
        .expect("feature1 commit should succeed");

    // Second feature branch from main
    repo.git(&["checkout", "main"])
        .expect("checkout main should succeed");
    repo.git(&["checkout", "-b", "feature2"])
        .expect("checkout feature2 should succeed");
    file.set_contents(lines!["line1".ai(), "feature2".ai()]);
    repo.stage_all_and_commit("Feature 2")
        .expect("feature2 commit should succeed");

    // Advance main
    repo.git(&["checkout", "main"])
        .expect("checkout main should succeed");
    let mut other = repo.filename("other.rs");
    other.set_contents(lines!["main advance".ai()]);
    repo.stage_all_and_commit("Main advance")
        .expect("main advance commit should succeed");

    let events_before = find_rebase_complete_events(&repo).len();

    // Rebase feature1
    repo.git(&["checkout", "feature1"])
        .expect("checkout feature1 for rebase should succeed");
    repo.git(&["rebase", "main"])
        .expect("feature1 rebase should succeed");
    let feature1_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("HEAD should exist after feature1 rebase")
        .trim()
        .to_string();

    // Rebase feature2
    repo.git(&["checkout", "feature2"])
        .expect("checkout feature2 for rebase should succeed");
    repo.git(&["rebase", "main"])
        .expect("feature2 rebase should succeed");
    let feature2_head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("HEAD should exist after feature2 rebase")
        .trim()
        .to_string();

    // Verify both have notes
    assert!(has_authorship_note(&repo, &feature1_head));
    assert!(has_authorship_note(&repo, &feature2_head));

    // Check event ordering in rewrite log
    let gitai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    let all_events = gitai_repo
        .storage
        .read_rewrite_events()
        .expect("read rewrite log");

    let rebase_events: Vec<_> = all_events
        .into_iter()
        .filter_map(|e| match e {
            RewriteLogEvent::RebaseComplete { rebase_complete } => {
                Some(rebase_complete.new_head.clone())
            }
            _ => None,
        })
        .collect();

    // Should have exactly 2 new rebase events
    assert_eq!(rebase_events.len(), events_before + 2);

    // The events should be in reverse chronological order (newest first)
    // because rewrite log prepends
    assert_eq!(rebase_events[0], feature2_head, "Most recent rebase first");
    assert_eq!(rebase_events[1], feature1_head, "Earlier rebase second");
}
