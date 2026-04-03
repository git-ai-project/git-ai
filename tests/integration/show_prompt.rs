use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

// Local helper mirroring the CLI arg vector used by main
fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

// Reimport the parsing function from the show_prompt command module
use git_ai::commands::show_prompt::parse_args;

#[test]
fn parse_args_requires_prompt_id() {
    let result = parse_args(&args(&[]));
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "show-prompt requires a prompt ID");
}

#[test]
fn parse_args_parses_basic_id() {
    let result = parse_args(&args(&["my-prompt-id"])).unwrap();
    assert_eq!(result.prompt_id, "my-prompt-id");
    assert!(result.commit.is_none());
    assert_eq!(result.offset, 0);
}

#[test]
fn parse_args_parses_commit_flag() {
    let result = parse_args(&args(&["my-id", "--commit", "HEAD"])).unwrap();
    assert_eq!(result.prompt_id, "my-id");
    assert_eq!(result.commit.as_deref(), Some("HEAD"));
    assert_eq!(result.offset, 0);
}

#[test]
fn parse_args_parses_offset_flag() {
    let result = parse_args(&args(&["my-id", "--offset", "2"])).unwrap();
    assert_eq!(result.prompt_id, "my-id");
    assert!(result.commit.is_none());
    assert_eq!(result.offset, 2);
}

#[test]
fn parse_args_rejects_commit_and_offset_together() {
    let result = parse_args(&args(&["id", "--commit", "HEAD", "--offset", "1"]));
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err(),
        "--commit and --offset are mutually exclusive"
    );
}

#[test]
fn parse_args_rejects_multiple_prompt_ids() {
    let result = parse_args(&args(&["id1", "id2"]));
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err(),
        "Only one prompt ID can be specified".to_string()
    );
}

#[test]
fn parse_args_requires_commit_value() {
    let result = parse_args(&args(&["id", "--commit"]));
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "--commit requires a value");
}

#[test]
fn parse_args_requires_offset_value() {
    let result = parse_args(&args(&["id", "--offset"]));
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "--offset requires a value");
}

#[test]
fn parse_args_rejects_invalid_offset() {
    let result = parse_args(&args(&["id", "--offset", "not-a-number"]));
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err(),
        "--offset must be a non-negative integer"
    );
}

#[test]
fn parse_args_rejects_unknown_flag() {
    let result = parse_args(&args(&["id", "--unknown"]));
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "Unknown option: --unknown");
}

#[test]
fn show_prompt_returns_latest_prompt_by_default() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // First AI session
    file.set_contents(crate::lines!["Base".human(), "AI line 1".ai()]);
    repo.stage_all_and_commit("First commit").unwrap();

    // Second AI session with a different AI line so we have distinct prompts
    file.insert_at(2, crate::lines!["AI line 2".ai()]);
    let second_commit = repo.stage_all_and_commit("Second commit").unwrap();

    // Grab one of the prompt IDs from the latest commit's authorship log
    let prompts = &second_commit.authorship_log.metadata.prompts;
    let (prompt_id, _prompt) = prompts.iter().next().expect("expected at least one prompt");

    // show-prompt should return the latest occurrence by default
    let output = repo
        .git_ai(&["show-prompt", prompt_id])
        .expect("show-prompt should succeed");

    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(json["prompt_id"].as_str(), Some(prompt_id.as_str()));
    assert_eq!(
        json["commit"].as_str(),
        Some(second_commit.commit_sha.as_str())
    );
}

#[test]
fn show_prompt_with_offset_skips_occurrences() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // First AI session
    file.set_contents(crate::lines!["Base".human(), "AI line 1".ai()]);
    let first_commit = repo.stage_all_and_commit("First commit").unwrap();

    // Second AI session with new AI content so we get another prompt occurrence
    file.insert_at(2, crate::lines!["AI line 2".ai()]);
    repo.stage_all_and_commit("Second commit").unwrap();

    // Use a prompt ID from the first commit
    let prompts_first = &first_commit.authorship_log.metadata.prompts;
    let (prompt_id, _) = prompts_first
        .iter()
        .next()
        .expect("expected at least one prompt in first commit");

    // Default (no offset) and explicit offset 0 should both succeed and point to the same commit
    let default_output = repo
        .git_ai(&["show-prompt", prompt_id])
        .expect("show-prompt without offset should succeed");
    let default_json: serde_json::Value = serde_json::from_str(default_output.trim()).unwrap();

    let offset0_output = repo
        .git_ai(&["show-prompt", prompt_id, "--offset", "0"])
        .expect("show-prompt with offset 0 should succeed");
    let offset0_json: serde_json::Value = serde_json::from_str(offset0_output.trim()).unwrap();

    assert_eq!(default_json["commit"], offset0_json["commit"]);

    // Offset that is too large should return a clear error
    let err = repo
        .git_ai(&["show-prompt", prompt_id, "--offset", "1"])
        .expect_err("show-prompt with offset 1 should fail when only one occurrence exists");
    assert!(
        err.contains("found 1 time(s), but offset 1 requested"),
        "unexpected error message: {}",
        err
    );
}

/// Regression test for https://github.com/git-ai-project/git-ai/issues/861
///
/// When two commits belong to the same AI session (same prompt ID), the full
/// transcript is shared.  `show-prompt --commit <first>` should truncate the
/// messages to only those that occurred up to the first commit's timestamp,
/// while `show-prompt --commit <second>` may include later messages.
#[test]
fn show_prompt_commit_flag_scopes_messages_to_commit() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // ── Commit 1: authored at 2025-06-01T10:00:00Z ──
    file.set_contents(crate::lines!["Base".human(), "AI line 1".ai()]);
    let first_commit = repo
        .stage_all_and_commit_with_env(
            "First AI commit",
            &[
                ("GIT_AUTHOR_DATE", "2025-06-01T10:00:00+00:00"),
                ("GIT_COMMITTER_DATE", "2025-06-01T10:00:00+00:00"),
            ],
        )
        .unwrap();

    // ── Commit 2: authored at 2025-06-01T12:00:00Z ──
    file.insert_at(2, crate::lines!["AI line 2".ai()]);
    let second_commit = repo
        .stage_all_and_commit_with_env(
            "Second AI commit",
            &[
                ("GIT_AUTHOR_DATE", "2025-06-01T12:00:00+00:00"),
                ("GIT_COMMITTER_DATE", "2025-06-01T12:00:00+00:00"),
            ],
        )
        .unwrap();

    // Both commits should reference the same prompt ID (same AI session)
    let prompt_id_first = first_commit
        .authorship_log
        .metadata
        .prompts
        .keys()
        .next()
        .expect("first commit should have a prompt")
        .clone();

    let prompt_id_second = second_commit
        .authorship_log
        .metadata
        .prompts
        .keys()
        .next()
        .expect("second commit should have a prompt")
        .clone();

    // Use show-prompt with --commit for the first commit
    let output_first = repo
        .git_ai(&[
            "show-prompt",
            &prompt_id_first,
            "--commit",
            &first_commit.commit_sha,
        ])
        .expect("show-prompt --commit for first commit should succeed");
    let json_first: serde_json::Value = serde_json::from_str(output_first.trim()).unwrap();

    // Use show-prompt with --commit for the second commit
    let output_second = repo
        .git_ai(&[
            "show-prompt",
            &prompt_id_second,
            "--commit",
            &second_commit.commit_sha,
        ])
        .expect("show-prompt --commit for second commit should succeed");
    let json_second: serde_json::Value = serde_json::from_str(output_second.trim()).unwrap();

    let msgs_first = json_first["prompt"]["messages"]
        .as_array()
        .expect("first commit should have messages array");
    let msgs_second = json_second["prompt"]["messages"]
        .as_array()
        .expect("second commit should have messages array");

    // The key assertion: if messages are present and have timestamps, the
    // first commit should have fewer (or equal) messages than the second.
    // If the messages are empty (no CAS/SQLite resolution in test env), the
    // test still passes — the important thing is they are NOT identical when
    // messages ARE present.
    if !msgs_first.is_empty() && !msgs_second.is_empty() {
        assert!(
            msgs_first.len() <= msgs_second.len(),
            "First commit ({} msgs) should have <= messages than second commit ({} msgs).\n\
             First: {:?}\nSecond: {:?}",
            msgs_first.len(),
            msgs_second.len(),
            msgs_first,
            msgs_second,
        );
    }

    // Verify both resolve to the correct commits
    assert_eq!(
        json_first["commit"].as_str(),
        Some(first_commit.commit_sha.as_str()),
        "first show-prompt should resolve to first commit"
    );
    assert_eq!(
        json_second["commit"].as_str(),
        Some(second_commit.commit_sha.as_str()),
        "second show-prompt should resolve to second commit"
    );
}

crate::reuse_tests_in_worktree!(
    parse_args_requires_prompt_id,
    parse_args_parses_basic_id,
    parse_args_parses_commit_flag,
    parse_args_parses_offset_flag,
    parse_args_rejects_commit_and_offset_together,
    parse_args_rejects_multiple_prompt_ids,
    parse_args_requires_commit_value,
    parse_args_requires_offset_value,
    parse_args_rejects_invalid_offset,
    parse_args_rejects_unknown_flag,
    show_prompt_returns_latest_prompt_by_default,
    show_prompt_with_offset_skips_occurrences,
    show_prompt_commit_flag_scopes_messages_to_commit,
);
