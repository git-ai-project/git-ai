use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log::LineRange;

#[test]
fn test_change_across_commits() {
    let repo = TestRepo::new();
    let mut file = repo.filename("foo.py");

    file.set_contents(crate::lines![
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
    file.insert_at(4, crate::lines!["    name = name.upper()".human()]);

    let commit = repo.stage_all_and_commit("add more AI").unwrap();

    let file_attestation = commit.authorship_log.attestations.first().unwrap();
    assert_eq!(file_attestation.entries.len(), 2);

    // With sessions format, verify that sessions exist in metadata
    // Sessions use unique IDs (based on timestamp), so each set_contents/replace_at creates new sessions
    assert!(
        !commit.authorship_log.metadata.sessions.is_empty(),
        "Should have session records in metadata"
    );

    // Find the entry for the new AI line (line 6)
    let second_ai_entry = file_attestation
        .entries
        .iter()
        .find(|e| {
            // The new AI line should be at line 6 (after human insertion at line 4)
            e.line_ranges.contains(&LineRange::Single(6))
        })
        .expect("Should find entry for new AI line at line 6");

    // Verify it's a different session than the initial one
    assert_ne!(second_ai_entry.hash, initial_ai_entry.hash);
}

/// Variant of test_change_across_commits using unattributed (legacy) human checkpoints.
/// Assertions match origin/main: with empty attribution, the file has only 1 attestation
/// entry (the second AI commit's entry only) because the first commit's attribution is
/// subsumed into the working log without creating a separate attestation entry.
#[test]
fn test_change_across_commits_standard_human() {
    let repo = TestRepo::new();
    let mut file = repo.filename("foo.py");

    file.set_contents(crate::lines![
        "def print_name(name: str) -> None:".ai(),
        "    \"\"\"Print the given name.\"\"\"".ai(),
        "    if name == 'foobar':".ai(),
        "        print('name not allowed!')".ai(),
        "    print(f\"Hello, {name}!\")".ai(),
        "".ai(),
        "print_name(\"Michael\")".ai(),
    ]);

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
    file.insert_at(
        4,
        crate::lines!["    name = name.upper()".unattributed_human()],
    );

    let commit = repo.stage_all_and_commit("add more AI").unwrap();

    let file_attestation = commit.authorship_log.attestations.first().unwrap();
    // Post-commit attribution recovery (AI edge extension) now absorbs the
    // untracked `name = name.upper()` line (line 5), which sits directly between
    // the AI line above (line 4) and the AI line below (line 6), into the second
    // AI commit's session. That yields a second attestation entry (line 5) on
    // top of the second AI commit's own entry (line 6). Both share the second
    // commit's session key but carry distinct trace ids.
    assert_eq!(file_attestation.entries.len(), 2);

    let second_ai_session_hash = commit
        .authorship_log
        .metadata
        .sessions
        .keys()
        .next()
        .unwrap();
    assert_ne!(*second_ai_session_hash, initial_ai_entry.hash);

    // The second AI commit's own entry covers the AI line at line 6.
    let second_ai_entry = file_attestation
        .entries
        .iter()
        .find(|e| e.line_ranges == vec![LineRange::Single(6)])
        .expect("Should find entry for AI line at line 6");
    assert_ne!(second_ai_entry.hash, initial_ai_entry.hash);

    // The recovered entry covers the absorbed untracked line at line 5.
    let recovered_entry = file_attestation
        .entries
        .iter()
        .find(|e| e.line_ranges == vec![LineRange::Single(5)])
        .expect("Should find recovered entry for absorbed line at line 5");
    assert_ne!(recovered_entry.hash, initial_ai_entry.hash);
}

crate::reuse_tests_in_worktree!(
    test_change_across_commits,
    test_change_across_commits_standard_human,
);
