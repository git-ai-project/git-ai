use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

#[test]
fn oversized_agent_config_is_rejected_and_attribution_recovers() {
    let repo = TestRepo::new();
    let mut file = repo.filename("tracked.txt");
    file.set_contents(lines!["first", "second", "third"]);
    repo.stage_all_and_commit("initial").unwrap();
    file.assert_committed_lines(lines!["first".human(), "second".human(), "third".human(),]);

    let claude_dir = repo.test_home_path().join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    let settings_path = claude_dir.join("settings.json");
    let settings = fs::File::create(&settings_path).unwrap();
    settings.set_len(2 * 1024 * 1024 + 1).unwrap();

    let output = repo.git_ai(&["install", "--dry-run"]).unwrap();
    assert!(
        output.contains("agent configuration exceeded the 2097152 byte limit"),
        "unexpected install output: {output}"
    );

    fs::remove_file(settings_path).unwrap();
    file.insert_at(3, lines!["AI edit".ai()]);
    repo.stage_all_and_commit("AI edit").unwrap();
    file.assert_committed_lines(lines![
        "first".human(),
        "second".human(),
        "third".ai(),
        "AI edit".ai(),
    ]);
}
