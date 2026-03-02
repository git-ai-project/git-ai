#[macro_use]
mod repos;
mod test_utils;

use crate::repos::test_repo::TestRepo;

// ==============================================================================
// Helper: extract the Git-AI trailer value from a commit message
// ==============================================================================

fn extract_git_ai_trailer(repo: &TestRepo, commit_sha: &str) -> Option<String> {
    // Use `git log` to read the raw commit message body for the given SHA,
    // then look for a line starting with "Git-AI: ".
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
    // UUID v4 format: 8-4-4-4-12 hex characters
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

// ==============================================================================
// Test 1: Basic commit has Git-AI trailer with valid UUID
// ==============================================================================

#[test]
fn test_commit_has_git_ai_trailer() {
    let repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["hello world"])
        .stage();

    let commit = repo.commit("test commit").unwrap();

    let trailer_value =
        extract_git_ai_trailer(&repo, &commit.commit_sha).expect("Git-AI trailer should exist");

    assert!(
        is_valid_uuid_v4(&trailer_value),
        "Trailer value '{}' should be a valid UUID v4",
        trailer_value
    );
}

// ==============================================================================
// Test 2: Authorship log metadata has id field
// ==============================================================================

#[test]
fn test_commit_authorship_log_has_id_field() {
    let repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["hello world"])
        .stage();

    let commit = repo.commit("test commit").unwrap();

    let note_id = commit
        .authorship_log
        .metadata
        .id
        .as_ref()
        .expect("Authorship log metadata should have an id field");

    assert!(
        is_valid_uuid_v4(note_id),
        "Note id '{}' should be a valid UUID v4",
        note_id
    );
}

// ==============================================================================
// Test 3: Trailer UUID matches note id
// ==============================================================================

#[test]
fn test_trailer_uuid_matches_note_id() {
    let repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["hello world"])
        .stage();

    let commit = repo.commit("test commit").unwrap();

    let trailer_value =
        extract_git_ai_trailer(&repo, &commit.commit_sha).expect("Git-AI trailer should exist");

    let note_id = commit
        .authorship_log
        .metadata
        .id
        .as_ref()
        .expect("Authorship log metadata should have an id field");

    assert_eq!(
        &trailer_value, note_id,
        "Trailer UUID and note id must match"
    );
}

// ==============================================================================
// Test 4: Multiple commits have unique UUIDs
// ==============================================================================

#[test]
fn test_multiple_commits_have_unique_uuids() {
    let repo = TestRepo::new();

    // First commit
    repo.filename("a.txt")
        .set_contents(vec!["first file"])
        .stage();
    let commit1 = repo.commit("first commit").unwrap();

    // Second commit
    repo.filename("b.txt")
        .set_contents(vec!["second file"])
        .stage();
    let commit2 = repo.commit("second commit").unwrap();

    let id1 = commit1
        .authorship_log
        .metadata
        .id
        .as_ref()
        .expect("First commit should have note id");
    let id2 = commit2
        .authorship_log
        .metadata
        .id
        .as_ref()
        .expect("Second commit should have note id");

    assert_ne!(id1, id2, "Each commit should have a unique UUID");

    let trailer1 =
        extract_git_ai_trailer(&repo, &commit1.commit_sha).expect("First trailer should exist");
    let trailer2 =
        extract_git_ai_trailer(&repo, &commit2.commit_sha).expect("Second trailer should exist");

    assert_ne!(
        trailer1, trailer2,
        "Each commit trailer should have a unique UUID"
    );
    assert_eq!(&trailer1, id1, "First trailer should match first note id");
    assert_eq!(&trailer2, id2, "Second trailer should match second note id");
}

// ==============================================================================
// Test 5: Amend commit has Git-AI trailer
// ==============================================================================

#[test]
fn test_amend_commit_has_git_ai_trailer() {
    let repo = TestRepo::new();

    // Initial commit
    repo.filename("test.txt")
        .set_contents(vec!["initial content"])
        .stage();
    let _initial = repo.commit("initial commit").unwrap();

    // Amend
    repo.filename("test.txt")
        .set_contents(vec!["amended content"])
        .stage();
    let amend_output = repo.git(&["commit", "--amend", "-m", "amended commit"]);
    assert!(amend_output.is_ok(), "Amend should succeed");

    // Read the amended commit's message via HEAD
    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let trailer_value = extract_git_ai_trailer(&repo, &head_sha)
        .expect("Amended commit should have Git-AI trailer");

    assert!(
        is_valid_uuid_v4(&trailer_value),
        "Amended trailer '{}' should be a valid UUID v4",
        trailer_value
    );
}

// ==============================================================================
// Test 6: Trailer follows correct git trailer format (parseable by git)
// ==============================================================================

#[test]
fn test_commit_trailer_format() {
    let repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["hello world"])
        .stage();

    let commit = repo.commit("test commit").unwrap();

    // Use git interpret-trailers --parse to verify the trailer is valid git format
    let parsed = repo
        .git_og(&[
            "log",
            "-1",
            "--format=%(trailers:key=Git-AI,valueonly)",
            &commit.commit_sha,
        ])
        .expect("git log with trailer format should succeed");

    let trailer_uuid = parsed.trim().to_string();
    assert!(
        !trailer_uuid.is_empty(),
        "Git should be able to parse the Git-AI trailer"
    );
    assert!(
        is_valid_uuid_v4(&trailer_uuid),
        "Parsed trailer value '{}' should be a valid UUID v4",
        trailer_uuid
    );
}

// ==============================================================================
// Test 7: Commit message preserves user content alongside trailer
// ==============================================================================

#[test]
fn test_commit_with_existing_message_preserves_content() {
    let repo = TestRepo::new();

    repo.filename("test.txt")
        .set_contents(vec!["hello world"])
        .stage();

    let commit = repo.commit("my custom message").unwrap();

    let full_message = repo
        .git_og(&["log", "-1", "--format=%B", &commit.commit_sha])
        .expect("git log should succeed");

    assert!(
        full_message.contains("my custom message"),
        "Commit message should contain the original message"
    );

    let trailer_value = extract_git_ai_trailer(&repo, &commit.commit_sha)
        .expect("Git-AI trailer should exist alongside custom message");

    assert!(
        is_valid_uuid_v4(&trailer_value),
        "Trailer should be valid UUID"
    );
}

// ==============================================================================
// Test 8: Empty repo first commit has trailer
// ==============================================================================

#[test]
fn test_empty_repo_first_commit_has_trailer() {
    let repo = TestRepo::new();

    // This IS the first commit in the repo (TestRepo::new creates an empty repo)
    repo.filename("first.txt")
        .set_contents(vec!["first file ever"])
        .stage();

    let commit = repo.commit("first commit").unwrap();

    let trailer_value = extract_git_ai_trailer(&repo, &commit.commit_sha)
        .expect("First commit should have trailer");

    assert!(
        is_valid_uuid_v4(&trailer_value),
        "First commit trailer '{}' should be valid UUID",
        trailer_value
    );

    let note_id = commit
        .authorship_log
        .metadata
        .id
        .as_ref()
        .expect("First commit note should have id");

    assert_eq!(
        &trailer_value, note_id,
        "First commit trailer and note id should match"
    );
}

// ==============================================================================
// Reuse tests in worktree mode
// ==============================================================================

reuse_tests_in_worktree!(
    test_commit_has_git_ai_trailer,
    test_commit_authorship_log_has_id_field,
    test_trailer_uuid_matches_note_id,
    test_multiple_commits_have_unique_uuids,
    test_amend_commit_has_git_ai_trailer,
    test_commit_trailer_format,
    test_commit_with_existing_message_preserves_content,
    test_empty_repo_first_commit_has_trailer,
);
