use crate::repos::test_repo::TestRepo;
use git_ai::authorship::attribution_tracker::LineAttribution;
use git_ai::authorship::authorship_log::HumanRecord;
use git_ai::daemon::{RecentWorkingLogSnapshot, restore_recent_working_log_snapshot};
use git_ai::git::find_repository_in_path;
use std::collections::{BTreeMap, HashMap};
use std::fs;

#[test]
fn recent_working_log_snapshot_preserves_humans_on_restore() {
    let repo = TestRepo::new();
    fs::write(repo.path().join("init.txt"), "init\n").unwrap();
    repo.git_og(&["add", "."]).unwrap();
    repo.git_og(&["commit", "-m", "initial commit"]).unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let h_hash = "h_abc123";
    let human_record = HumanRecord {
        author: "Test User <test@example.com>".to_string(),
    };

    let file_path = "test.txt";
    let line_attributions = vec![LineAttribution {
        start_line: 1,
        end_line: 1,
        author_id: h_hash.to_string(),
        overrode: None,
    }];

    let mut humans = BTreeMap::new();
    humans.insert(h_hash.to_string(), human_record.clone());

    let snapshot = RecentWorkingLogSnapshot {
        files: HashMap::from([(file_path.to_string(), line_attributions.clone())]),
        prompts: HashMap::new(),
        file_contents: HashMap::from([(file_path.to_string(), "test line\n".to_string())]),
        humans: humans.clone(),
        sessions: BTreeMap::new(),
    };

    let base_commit = "HEAD";
    let restored =
        restore_recent_working_log_snapshot(&gitai_repo, base_commit, &snapshot).unwrap();
    assert!(restored, "Snapshot should be restored");

    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(base_commit)
        .unwrap();
    let initial = working_log.read_initial_attributions();

    assert_eq!(
        initial.humans.len(),
        1,
        "Should have one human record after restore"
    );
    assert_eq!(
        initial.humans.get(h_hash),
        Some(&human_record),
        "Human record should match"
    );
}
