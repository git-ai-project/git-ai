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
