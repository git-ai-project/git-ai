#[macro_use]
mod repos;
mod test_utils;

use git_ai::authorship::authorship_log::LineRange;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

#[test]
fn test_change_across_commits() {
    let repo = TestRepo::new();
    let mut file = repo.filename("foo.py");

    file.set_contents(lines![
        "def print_name(name: str) -> None:".ai(),
        "    \"\"\"Print the given name.\"\"\"".ai(),
        "    if name == 'foobar':".ai(),
        "        print('name not allowed!')".ai(),
        "    print(f\"Hello, {name}!\")".ai(),
        "".ai(),
        "print_name(\"Michael\")".ai(),
    ]);
    println!(
        "file: {}",
        file.lines
            .iter()
            .map(|line| line.contents.clone())
            .collect::<Vec<String>>()
            .join("\n")
    );

    let commit = repo.stage_all_and_commit("Initial all AI").unwrap();
    let initial_ai_entry = commit
        .authorship_log
        .attestations
        .first()
        .unwrap()
        .entries
        .first()
        .unwrap();

    file.replace_at(4, "    print(f\"Hello, {name.upper()}!\")".ai());
    file.insert_at(4, lines!["    name = name.upper()".human()]);

    let commit = repo.stage_all_and_commit("add more AI").unwrap();

    let file_attestation = commit.authorship_log.attestations.first().unwrap();
    assert_eq!(file_attestation.entries.len(), 1);

    let second_ai_prompt_hash = commit
        .authorship_log
        .metadata
        .prompts
        .keys()
        .next()
        .unwrap();
    assert_ne!(*second_ai_prompt_hash, initial_ai_entry.hash);

    let second_ai_entry = file_attestation.entries.first().unwrap();
    assert_eq!(second_ai_entry.line_ranges, vec![LineRange::Single(6)]);
    assert_ne!(second_ai_entry.hash, initial_ai_entry.hash);
}

worktree_test_wrappers! {
    test_change_across_commits,
}
