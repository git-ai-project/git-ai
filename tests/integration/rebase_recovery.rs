use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::rebase_recovery;
use git_ai::git::repository::find_repository_in_path;

/// Test that a rebase snapshot is created automatically during rebase.
#[test]
fn test_rebase_creates_recovery_snapshot() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("code.txt");
    file.set_contents(crate::lines!["line 1".ai(), "line 2".ai()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI content
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["ai feature line".ai()]);
    repo.stage_all_and_commit("Feature commit").unwrap();

    // Advance main with a new commit
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main_new.txt");
    main_file.set_contents(crate::lines!["main advance"]);
    repo.stage_all_and_commit("Main advance").unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Force daemon sync (notes command triggers sync)
    let _ = repo.git(&["notes", "--ref=ai", "list"]);

    // Verify snapshot was created
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let snapshots = rebase_recovery::list_snapshots(&gitai_repo.storage);
    assert!(
        !snapshots.is_empty(),
        "Expected at least one recovery snapshot after rebase"
    );

    let snapshot = &snapshots[0];
    assert!(
        !snapshot.note_entries.is_empty(),
        "Snapshot should contain note entries"
    );
    assert_eq!(
        snapshot.original_commits.len(),
        1,
        "Should have 1 original commit in snapshot"
    );
}

/// Test that recovery snapshot preserves correct note content.
#[test]
fn test_rebase_snapshot_preserves_note_content() {
    let repo = TestRepo::new();

    // Create initial commit on main
    let mut file = repo.filename("code.txt");
    file.set_contents(crate::lines!["line 1".ai(), "line 2".ai()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI content
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["ai feature".ai()]);
    repo.stage_all_and_commit("Feature commit").unwrap();

    let feature_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    // Force daemon sync to ensure note is written
    let _ = repo.git(&["notes", "--ref=ai", "list"]);
    let feature_note = repo.read_authorship_note(&feature_sha).unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main_new.txt");
    main_file.set_contents(crate::lines!["main advance"]);
    repo.stage_all_and_commit("Main advance").unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Force daemon sync
    let _ = repo.git(&["notes", "--ref=ai", "list"]);

    // Verify snapshot contains the pre-rebase note content
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let snapshots = rebase_recovery::list_snapshots(&gitai_repo.storage);
    assert!(!snapshots.is_empty());

    let snapshot = &snapshots[0];
    assert!(
        snapshot.note_entries.contains_key(&feature_sha),
        "Snapshot should contain the original feature commit's note"
    );
    let stored_note = &snapshot.note_entries[&feature_sha];
    assert_eq!(
        stored_note, &feature_note,
        "Stored note should match original note content"
    );
}

/// Test that snapshot pruning keeps only 5 snapshots.
#[test]
fn test_rebase_snapshot_pruning() {
    let repo = TestRepo::new();

    // Create initial content
    let mut file = repo.filename("code.txt");
    file.set_contents(crate::lines!["line 1".ai()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Perform multiple rebases to create multiple snapshots
    for i in 0..7 {
        repo.git(&["checkout", "-b", &format!("feature-{}", i)])
            .unwrap();
        let mut f = repo.filename(&format!("f{}.txt", i));
        f.set_contents(crate::lines![format!("ai line {}", i).as_str().ai()]);
        repo.stage_all_and_commit(&format!("Feature {}", i))
            .unwrap();

        // Advance main
        repo.git(&["checkout", &default_branch]).unwrap();
        let mut m = repo.filename(&format!("m{}.txt", i));
        m.set_contents(crate::lines![format!("main {}", i).as_str()]);
        repo.stage_all_and_commit(&format!("Main advance {}", i))
            .unwrap();

        // Rebase
        repo.git(&["checkout", &format!("feature-{}", i)]).unwrap();
        repo.git(&["rebase", &default_branch]).unwrap();

        // Force daemon sync
        let _ = repo.git(&["notes", "--ref=ai", "list"]);

        // Return to main for next iteration
        repo.git(&["checkout", &default_branch]).unwrap();
        repo.git(&["merge", &format!("feature-{}", i)]).unwrap();
    }

    // Verify at most 5 snapshots exist
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let snapshots = rebase_recovery::list_snapshots(&gitai_repo.storage);
    assert!(
        snapshots.len() <= 5,
        "Should have at most 5 snapshots, got {}",
        snapshots.len()
    );
}

/// Test that list_snapshots returns empty when no snapshots exist.
#[test]
fn test_rebase_recovery_list_empty() {
    let repo = TestRepo::new();

    let mut file = repo.filename("code.txt");
    file.set_contents(crate::lines!["line 1"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let snapshots = rebase_recovery::list_snapshots(&gitai_repo.storage);
    assert!(snapshots.is_empty(), "Should have no snapshots initially");
}

/// Test loading a snapshot by timestamp.
#[test]
fn test_rebase_recovery_load_by_timestamp() {
    let repo = TestRepo::new();

    // Create content with AI attribution
    let mut file = repo.filename("code.txt");
    file.set_contents(crate::lines!["line 1".ai()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create and rebase feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut f = repo.filename("feature.txt");
    f.set_contents(crate::lines!["ai feature".ai()]);
    repo.stage_all_and_commit("Feature commit").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    let mut m = repo.filename("main_new.txt");
    m.set_contents(crate::lines!["main advance"]);
    repo.stage_all_and_commit("Main advance").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Force daemon sync
    let _ = repo.git(&["notes", "--ref=ai", "list"]);

    // Get the snapshot timestamp
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let snapshots = rebase_recovery::list_snapshots(&gitai_repo.storage);
    assert!(!snapshots.is_empty());
    let timestamp = snapshots[0].timestamp;

    // Load by timestamp
    let loaded = rebase_recovery::load_snapshot_by_timestamp(&gitai_repo.storage, timestamp);
    assert!(
        loaded.is_some(),
        "Should be able to load snapshot by timestamp"
    );
    assert_eq!(loaded.unwrap().timestamp, timestamp);

    // Loading nonexistent timestamp returns None
    let missing = rebase_recovery::load_snapshot_by_timestamp(&gitai_repo.storage, 12345);
    assert!(missing.is_none());
}
