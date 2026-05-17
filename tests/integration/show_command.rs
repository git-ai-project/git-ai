use crate::repos::test_repo::TestRepo;
use std::fs;

#[test]
fn test_show_no_args_fails() {
    let repo = TestRepo::new();
    let result = repo.git_ai(&["show"]);
    assert!(result.is_err(), "show with no args should fail");
}

#[test]
fn test_show_too_many_args_fails() {
    let repo = TestRepo::new();
    fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    repo.stage_all_and_commit("init").unwrap();
    let result = repo.git_ai(&["show", "HEAD", "HEAD~1"]);
    assert!(result.is_err(), "show with multiple args should fail");
}

#[test]
fn test_show_invalid_revision_fails() {
    let repo = TestRepo::new();
    fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    repo.stage_all_and_commit("init").unwrap();
    let result = repo.git_ai(&["show", "nonexistent_ref_abc123"]);
    assert!(result.is_err(), "show with invalid revision should fail");
}

#[test]
fn test_show_commit_without_note() {
    let repo = TestRepo::new();
    fs::write(repo.path().join("a.txt"), "hello\n").unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "no authorship"]).unwrap();

    let output = repo
        .git_ai(&["show", "HEAD"])
        .expect("show should succeed even without note");
    assert!(
        output.contains("No authorship data"),
        "should report no authorship data"
    );
}

#[test]
fn test_show_commit_with_authorship_note() {
    let repo = TestRepo::new();
    fs::write(repo.path().join("a.txt"), "hello\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    fs::write(repo.path().join("a.txt"), "ai was here\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "a.txt"])
        .expect("checkpoint should succeed");

    let commit = repo.stage_all_and_commit("ai commit").unwrap();
    let output = repo
        .git_ai(&["show", &commit.commit_sha])
        .expect("show should succeed");
    assert!(
        !output.contains("No authorship data"),
        "should have authorship data for AI commit"
    );
}

#[test]
fn test_show_range() {
    let repo = TestRepo::new();
    fs::write(repo.path().join("a.txt"), "v1\n").unwrap();
    repo.stage_all_and_commit("first").unwrap();
    let first_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    fs::write(repo.path().join("a.txt"), "v2\n").unwrap();
    repo.stage_all_and_commit("second").unwrap();

    fs::write(repo.path().join("a.txt"), "v3\n").unwrap();
    repo.stage_all_and_commit("third").unwrap();

    let range = format!("{}..HEAD", first_sha);
    let output = repo
        .git_ai(&["show", &range])
        .expect("show range should succeed");
    // Range from first..HEAD should show 2 commits (second and third)
    // Each commit's note contains schema_version, so count those
    let note_count = output.matches("schema_version").count();
    assert!(
        note_count >= 2,
        "range should cover multiple commits with authorship, got: {}",
        output
    );
}
