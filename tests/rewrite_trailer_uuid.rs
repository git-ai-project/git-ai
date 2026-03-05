#[macro_use]
mod repos;
mod test_utils;

use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::git::repository;

// ==============================================================================
// Helper: extract the Git-AI trailer value from a commit message
// ==============================================================================

fn extract_git_ai_trailer(repo: &TestRepo, commit_sha: &str) -> Option<String> {
    let output = repo
        .git_og(&["log", "-1", "--format=%B", commit_sha])
        .expect("git log should succeed");

    for line in output.lines() {
        if let Some(value) = line.strip_prefix("Git-AI: ") {
            return Some(value.trim().to_string());
        }
    }
    None
}

fn is_valid_uuid_v4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    if parts[0].len() != 8
        || parts[1].len() != 4
        || parts[2].len() != 4
        || parts[3].len() != 4
        || parts[4].len() != 12
    {
        return false;
    }
    parts
        .iter()
        .all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Read the authorship note for a given commit SHA and return as AuthorshipLog
fn read_note_for_commit(repo: &TestRepo, commit_sha: &str) -> Option<AuthorshipLog> {
    let ai_repo = repository::find_repository_in_path(repo.path().to_str().unwrap()).ok()?;
    let content = git_ai::git::refs::show_authorship_note(&ai_repo, commit_sha)?;
    AuthorshipLog::deserialize_from_string(&content).ok()
}

// ==============================================================================
// REBASE: Note id is preserved after rebase
// ==============================================================================

#[test]
fn test_rebase_preserves_note_id() {
    let repo = TestRepo::new();

    // Create initial commit on default branch
    let mut file = repo.filename("base.txt");
    file.set_contents(vec!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch with AI-authored commit
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(vec!["feature content"]);
    let feature_commit = repo.stage_all_and_commit("AI feature").unwrap();

    // Record the original note id
    let original_note_id = feature_commit
        .authorship_log
        .metadata
        .id
        .clone()
        .expect("Original commit should have note id");
    let original_sha = feature_commit.commit_sha.clone();

    // Verify original commit has a trailer
    let original_trailer = extract_git_ai_trailer(&repo, &original_sha)
        .expect("Original commit should have Git-AI trailer");
    assert_eq!(
        original_trailer, original_note_id,
        "Original trailer should match original note id"
    );

    // Advance main branch (creates divergence for rebase)
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut main_file = repo.filename("main-only.txt");
    main_file.set_contents(vec!["main content"]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &main_branch])
        .expect("Rebase should succeed");

    // Get the new commit SHA after rebase
    let new_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    assert_ne!(new_sha, original_sha, "Rebase should create a new commit");

    // Read the note on the rebased commit
    let rebased_note =
        read_note_for_commit(&repo, &new_sha).expect("Rebased commit should have authorship note");

    // The note should preserve the original id
    let rebased_note_id = rebased_note
        .metadata
        .id
        .as_ref()
        .expect("Rebased note should have id field");

    assert_eq!(
        rebased_note_id, &original_note_id,
        "Rebased note id should match original note id"
    );
}

// ==============================================================================
// REBASE: Multiple commits preserve their respective note ids
// ==============================================================================

#[test]
fn test_rebase_preserves_note_ids_multiple_commits() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("base.txt");
    file.set_contents(vec!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let main_branch = repo.current_branch();

    // Feature branch with 3 commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut f1 = repo.filename("f1.txt");
    f1.set_contents(vec!["feature 1"]);
    let c1 = repo.stage_all_and_commit("Feature 1").unwrap();
    let id1 = c1.authorship_log.metadata.id.clone().unwrap();

    let mut f2 = repo.filename("f2.txt");
    f2.set_contents(vec!["feature 2"]);
    let c2 = repo.stage_all_and_commit("Feature 2").unwrap();
    let id2 = c2.authorship_log.metadata.id.clone().unwrap();

    let mut f3 = repo.filename("f3.txt");
    f3.set_contents(vec!["feature 3"]);
    let c3 = repo.stage_all_and_commit("Feature 3").unwrap();
    let id3 = c3.authorship_log.metadata.id.clone().unwrap();

    // Advance main
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut m = repo.filename("main-file.txt");
    m.set_contents(vec!["main"]);
    repo.stage_all_and_commit("Main advance").unwrap();

    // Rebase
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &main_branch])
        .expect("Rebase should succeed");

    // Get the 3 new commit SHAs (HEAD~2, HEAD~1, HEAD)
    let new_sha3 = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let new_sha2 = repo
        .git_og(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    let new_sha1 = repo
        .git_og(&["rev-parse", "HEAD~2"])
        .unwrap()
        .trim()
        .to_string();

    // Verify each rebased commit preserves its original note id
    let note1 = read_note_for_commit(&repo, &new_sha1).expect("Rebased commit 1 should have note");
    let note2 = read_note_for_commit(&repo, &new_sha2).expect("Rebased commit 2 should have note");
    let note3 = read_note_for_commit(&repo, &new_sha3).expect("Rebased commit 3 should have note");

    assert_eq!(
        note1.metadata.id.as_ref().unwrap(),
        &id1,
        "Rebased commit 1 should preserve original id"
    );
    assert_eq!(
        note2.metadata.id.as_ref().unwrap(),
        &id2,
        "Rebased commit 2 should preserve original id"
    );
    assert_eq!(
        note3.metadata.id.as_ref().unwrap(),
        &id3,
        "Rebased commit 3 should preserve original id"
    );
}

// ==============================================================================
// CHERRY-PICK: Note id is preserved after cherry-pick
// ==============================================================================

#[test]
fn test_cherry_pick_preserves_note_id() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("base.txt");
    file.set_contents(vec!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch with AI-authored commit
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(vec!["feature content"]);
    let feature_commit = repo.stage_all_and_commit("AI feature").unwrap();

    let original_note_id = feature_commit
        .authorship_log
        .metadata
        .id
        .clone()
        .expect("Original commit should have note id");
    let feature_sha = feature_commit.commit_sha.clone();

    // Advance main (creates divergence so cherry-pick produces a new SHA)
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut main_file = repo.filename("main-diverge.txt");
    main_file.set_contents(vec!["main diverge"]);
    repo.stage_all_and_commit("Main diverges").unwrap();

    // Cherry-pick the feature commit onto diverged main
    repo.git(&["cherry-pick", &feature_sha])
        .expect("Cherry-pick should succeed");

    // Get the new commit SHA (HEAD after cherry-pick)
    let new_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    assert_ne!(
        new_sha, feature_sha,
        "Cherry-pick should create a new commit"
    );

    // Read the note on the cherry-picked commit
    let picked_note = read_note_for_commit(&repo, &new_sha)
        .expect("Cherry-picked commit should have authorship note");

    let picked_note_id = picked_note
        .metadata
        .id
        .as_ref()
        .expect("Cherry-picked note should have id field");

    assert_eq!(
        picked_note_id, &original_note_id,
        "Cherry-picked note id should match original note id"
    );
}

// ==============================================================================
// CHERRY-PICK: Multiple commits preserve their respective note ids
// ==============================================================================

#[test]
fn test_cherry_pick_multiple_preserves_note_ids() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("base.txt");
    file.set_contents(vec!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let main_branch = repo.current_branch();

    // Feature branch with 2 commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut f1 = repo.filename("pick1.txt");
    f1.set_contents(vec!["pick 1"]);
    let c1 = repo.stage_all_and_commit("Pick 1").unwrap();
    let id1 = c1.authorship_log.metadata.id.clone().unwrap();
    let sha1 = c1.commit_sha.clone();

    let mut f2 = repo.filename("pick2.txt");
    f2.set_contents(vec!["pick 2"]);
    let c2 = repo.stage_all_and_commit("Pick 2").unwrap();
    let id2 = c2.authorship_log.metadata.id.clone().unwrap();
    let sha2 = c2.commit_sha.clone();

    // Advance main (creates divergence so cherry-pick produces new SHAs)
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut m = repo.filename("main-diverge.txt");
    m.set_contents(vec!["main diverge"]);
    repo.stage_all_and_commit("Main diverges").unwrap();

    // Cherry-pick both
    repo.git(&["cherry-pick", &sha1, &sha2])
        .expect("Cherry-pick multiple should succeed");

    // HEAD is pick2, HEAD~1 is pick1
    let new_sha2 = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let new_sha1 = repo
        .git_og(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();

    let note1 =
        read_note_for_commit(&repo, &new_sha1).expect("Cherry-picked commit 1 should have note");
    let note2 =
        read_note_for_commit(&repo, &new_sha2).expect("Cherry-picked commit 2 should have note");

    assert_eq!(
        note1.metadata.id.as_ref().unwrap(),
        &id1,
        "Cherry-picked commit 1 should preserve original id"
    );
    assert_eq!(
        note2.metadata.id.as_ref().unwrap(),
        &id2,
        "Cherry-picked commit 2 should preserve original id"
    );
}

// ==============================================================================
// SQUASH-AUTHORSHIP (CI path): Note gets an id assigned
// ==============================================================================

#[test]
fn test_squash_merge_authorship_gets_note_id() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("base.txt");
    file.set_contents(vec!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let main_branch = repo.current_branch();

    // Feature branch with AI commit
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(vec!["feature content"]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Switch to main and do a squash merge
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"])
        .expect("Merge --squash should succeed");
    // Commit the squash merge (normal commit flow kicks in)
    let squash_commit = repo.commit("Squash merge feature").unwrap();

    let merge_sha = squash_commit.commit_sha.clone();

    // The squash merge commit should have a note id (from the normal commit flow)
    let note_id = squash_commit
        .authorship_log
        .metadata
        .id
        .as_ref()
        .expect("Squash merge commit should have note id");

    assert!(
        is_valid_uuid_v4(note_id),
        "Squash merge note id should be valid UUID"
    );

    // And a matching trailer
    let trailer = extract_git_ai_trailer(&repo, &merge_sha)
        .expect("Squash merge commit should have Git-AI trailer");
    assert_eq!(
        &trailer, note_id,
        "Squash merge trailer should match note id"
    );
}

// ==============================================================================
// CI SQUASH/REBASE: rewrite_authorship_after_squash_or_rebase produces note with id
// ==============================================================================

#[test]
fn test_squash_authorship_rewrite_gets_note_id() {
    // This simulates the CI path where rewrite_authorship_after_squash_or_rebase is called
    // directly (not through the normal commit flow).
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("base.txt");
    file.set_contents(vec!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let main_branch = repo.current_branch();

    // Feature branch with AI-authored commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut f1 = repo.filename("feat.txt");
    f1.set_contents(vec!["feat content"]);
    let c1 = repo.stage_all_and_commit("Feature commit").unwrap();
    let source_sha = c1.commit_sha.clone();

    // Switch to main, make a real merge commit (to simulate a CI squash merge)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();

    // Use git_og to make a commit bypassing git-ai hooks (simulating what CI does)
    repo.git_og(&["commit", "-m", "CI merge commit"]).unwrap();
    let merge_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Now call the CI-style rewrite function directly
    let ai_repo =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).expect("find repo");

    git_ai::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase(
        &ai_repo,
        &main_branch,
        "feature",
        &source_sha,
        &merge_sha,
        false,
    )
    .expect("rewrite_authorship_after_squash_or_rebase should succeed");

    // Read the note on the merge commit
    let note = read_note_for_commit(&repo, &merge_sha)
        .expect("CI merge commit should have authorship note");

    // The note should have an id
    let note_id = note
        .metadata
        .id
        .as_ref()
        .expect("CI squash/rebase note should have an id");

    assert!(
        is_valid_uuid_v4(note_id),
        "CI note id '{}' should be a valid UUID",
        note_id
    );
}

// ==============================================================================
// CI REBASE: rewrite_authorship_after_rebase_v2 preserves note ids from source
// ==============================================================================

#[test]
fn test_rebase_v2_rewrite_preserves_note_ids() {
    // This simulates the CI path for rebase merges
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("base.txt");
    file.set_contents(vec!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let main_branch = repo.current_branch();

    // Feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut f1 = repo.filename("feat1.txt");
    f1.set_contents(vec!["feat1"]);
    let c1 = repo.stage_all_and_commit("Feature 1").unwrap();
    let id1 = c1.authorship_log.metadata.id.clone().unwrap();
    let sha1 = c1.commit_sha.clone();

    let mut f2 = repo.filename("feat2.txt");
    f2.set_contents(vec!["feat2"]);
    let c2 = repo.stage_all_and_commit("Feature 2").unwrap();
    let id2 = c2.authorship_log.metadata.id.clone().unwrap();
    let sha2 = c2.commit_sha.clone();

    // Advance main
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut m = repo.filename("main-only.txt");
    m.set_contents(vec!["main"]);
    repo.stage_all_and_commit("Main advance").unwrap();
    let original_head = repo
        .git_og(&["rev-parse", "feature"])
        .unwrap()
        .trim()
        .to_string();

    // Simulate rebase by cherry-picking with git_og (bypass hooks) to create new commits
    repo.git_og(&["checkout", "-b", "rebased-feature"]).unwrap();
    repo.git_og(&["cherry-pick", &sha1, &sha2]).unwrap();

    let new_sha2 = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let new_sha1 = repo
        .git_og(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();

    // Call rewrite_authorship_after_rebase_v2 directly (CI path)
    let ai_repo =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).expect("find repo");

    git_ai::authorship::rebase_authorship::rewrite_authorship_after_rebase_v2(
        &ai_repo,
        &original_head,
        &[sha1.clone(), sha2.clone()],
        &[new_sha1.clone(), new_sha2.clone()],
        "",
    )
    .expect("rewrite_authorship_after_rebase_v2 should succeed");

    // Verify notes on new commits have preserved ids
    let note1 = read_note_for_commit(&repo, &new_sha1).expect("Rebased commit 1 should have note");
    let note2 = read_note_for_commit(&repo, &new_sha2).expect("Rebased commit 2 should have note");

    assert_eq!(
        note1.metadata.id.as_ref().unwrap(),
        &id1,
        "CI rebase note 1 should preserve original id"
    );
    assert_eq!(
        note2.metadata.id.as_ref().unwrap(),
        &id2,
        "CI rebase note 2 should preserve original id"
    );
}

// ==============================================================================
// CI CHERRY-PICK: rewrite_authorship_after_cherry_pick preserves note ids
// ==============================================================================

#[test]
fn test_cherry_pick_rewrite_preserves_note_ids() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("base.txt");
    file.set_contents(vec!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let main_branch = repo.current_branch();

    // Feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut f1 = repo.filename("cp1.txt");
    f1.set_contents(vec!["cherry 1"]);
    let c1 = repo.stage_all_and_commit("Cherry 1").unwrap();
    let id1 = c1.authorship_log.metadata.id.clone().unwrap();
    let sha1 = c1.commit_sha.clone();

    // Switch to main and create new commits via git_og (bypass hooks, simulates CI)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git_og(&["cherry-pick", &sha1]).unwrap();

    let new_sha1 = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Call rewrite_authorship_after_cherry_pick directly
    let ai_repo =
        repository::find_repository_in_path(repo.path().to_str().unwrap()).expect("find repo");

    git_ai::authorship::rebase_authorship::rewrite_authorship_after_cherry_pick(
        &ai_repo,
        &[sha1.clone()],
        &[new_sha1.clone()],
        "",
    )
    .expect("rewrite_authorship_after_cherry_pick should succeed");

    let note1 =
        read_note_for_commit(&repo, &new_sha1).expect("Cherry-picked commit should have note");

    assert_eq!(
        note1.metadata.id.as_ref().unwrap(),
        &id1,
        "Cherry-pick rewrite should preserve original note id"
    );
}

// ==============================================================================
// Reuse tests in worktree mode
// ==============================================================================

reuse_tests_in_worktree!(
    test_rebase_preserves_note_id,
    test_cherry_pick_preserves_note_id,
    test_squash_merge_authorship_gets_note_id,
);
