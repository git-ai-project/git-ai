use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

#[test]
fn oversized_blame_contents_file_is_rejected_and_attribution_recovers() {
    let repo = TestRepo::new();
    let mut file = repo.filename("tracked.txt");
    file.set_contents(lines!["first", "second", "third"]);
    repo.stage_all_and_commit("initial").unwrap();
    file.assert_committed_lines(lines!["first".human(), "second".human(), "third".human(),]);

    let contents_path = repo.test_home_path().join("oversized-contents.txt");
    let contents = fs::File::create(&contents_path).unwrap();
    contents.set_len(32 * 1024 * 1024 + 1).unwrap();
    let contents_path = contents_path.to_string_lossy().to_string();

    let error = repo
        .git_ai(&["blame", "--contents", &contents_path, "tracked.txt"])
        .expect_err("oversized blame contents must fail");
    assert!(
        error.contains("blame contents file exceeded the 33554432 byte limit"),
        "unexpected blame error: {error}"
    );

    file.insert_at(3, lines!["AI recovery".ai()]);
    repo.stage_all_and_commit("AI recovery").unwrap();
    file.assert_committed_lines(lines![
        "first".human(),
        "second".human(),
        "third".ai(),
        "AI recovery".ai(),
    ]);
}

#[test]
fn oversized_blame_contents_stdin_reports_one_error_and_attribution_recovers() {
    let repo = TestRepo::new();
    let tracked_path = repo.path().join("tracked.txt");
    fs::write(&tracked_path, "base\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    let oversized_input = vec![b'x'; 32 * 1024 * 1024 + 1];
    let error = repo
        .git_ai_with_stdin(
            &["blame", "--contents", "-", "tracked.txt"],
            &oversized_input,
        )
        .expect_err("oversized blame stdin must fail");
    assert_eq!(
        error.matches("Generic error:").count(),
        1,
        "blame stdin error must not be wrapped twice: {error}"
    );
    assert!(
        error.contains(
            "Failed to parse blame arguments: Generic error: blame contents stdin exceeded the \
             33554432 byte limit (33554433)"
        ),
        "unexpected blame stdin error: {error}"
    );
    drop(oversized_input);

    fs::write(&tracked_path, "base\nAI recovery\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI recovery").unwrap();
    file.assert_committed_lines(lines!["base".unattributed_human(), "AI recovery".ai(),]);
}
