use crate::repos::test_repo::TestRepo;
use std::fs;

/// Extract the JSON object from combined stdout+stderr output.
/// The JSON is written to stdout, but test infra combines stdout and stderr.
fn extract_json(output: &str) -> serde_json::Value {
    for line in output.lines().rev() {
        let trimmed = line.trim();
        if trimmed.starts_with('{')
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed)
        {
            return val;
        }
    }
    panic!("no valid JSON object found in output:\n{}", output);
}

#[test]
fn test_fetch_notes_no_remote_notes() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    fs::write(mirror.path().join("hello.txt"), "hello\n").expect("should write file");
    mirror
        .stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    let output = mirror
        .git_ai(&["fetch-notes"])
        .expect("fetch-notes should succeed");
    assert!(
        output.contains("no notes found on remote"),
        "expected 'no notes found' message, got: {}",
        output
    );
}

#[test]
fn test_fetch_notes_with_explicit_remote() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    fs::write(mirror.path().join("hello.txt"), "hello\n").expect("should write file");
    mirror
        .stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    let output = mirror
        .git_ai(&["fetch-notes", "origin"])
        .expect("fetch-notes with remote should succeed");
    assert!(
        output.contains("no notes found on remote"),
        "expected 'no notes found' message, got: {}",
        output
    );
}

#[test]
fn test_fetch_notes_with_remote_flag() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    fs::write(mirror.path().join("hello.txt"), "hello\n").expect("should write file");
    mirror
        .stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    let output = mirror
        .git_ai(&["fetch-notes", "--remote", "origin"])
        .expect("fetch-notes --remote should succeed");
    assert!(
        output.contains("no notes found on remote"),
        "expected 'no notes found' message, got: {}",
        output
    );
}

#[test]
fn test_fetch_notes_json_output_no_notes() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    fs::write(mirror.path().join("hello.txt"), "hello\n").expect("should write file");
    mirror
        .stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    let output = mirror
        .git_ai(&["fetch-notes", "--json"])
        .expect("fetch-notes --json should succeed");

    let parsed = extract_json(&output);
    assert_eq!(parsed["status"], "not_found");
    assert_eq!(parsed["remote"], "origin");
}

#[test]
fn test_fetch_notes_json_output_with_notes() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    fs::write(mirror.path().join("hello.txt"), "hello\n").expect("should write file");
    mirror
        .stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create a note on the commit and push it to the bare upstream.
    // Use -f in case git-ai hooks already created a note on this commit.
    mirror
        .git_og(&[
            "notes",
            "--ref=ai",
            "add",
            "-f",
            "-m",
            "test authorship note",
            "HEAD",
        ])
        .expect("should add note");
    mirror
        .git_og(&["push", "origin", "refs/notes/ai"])
        .expect("should push notes to upstream");

    // Remove the local note so fetch actually has something to pull
    mirror
        .git_og(&["update-ref", "-d", "refs/notes/ai"])
        .expect("should delete local note ref");

    let output = mirror
        .git_ai(&["fetch-notes", "--json"])
        .expect("fetch-notes --json should succeed");

    let parsed = extract_json(&output);
    assert_eq!(parsed["status"], "found");
    assert_eq!(parsed["remote"], "origin");
}

#[test]
fn test_fetch_notes_help_flag() {
    let repo = TestRepo::new();

    let output = repo
        .git_ai(&["fetch-notes", "--help"])
        .expect("fetch-notes --help should succeed");
    assert!(
        output.contains("Synchronously fetch AI authorship notes"),
        "help output should contain description, got: {}",
        output
    );
}

#[test]
fn test_fetch_notes_unknown_option_fails() {
    let repo = TestRepo::new();

    let err = repo
        .git_ai(&["fetch-notes", "--invalid-flag"])
        .expect_err("unknown option should fail");
    assert!(
        err.contains("unknown option"),
        "error should mention unknown option, got: {}",
        err
    );
}

#[test]
fn test_fetch_notes_human_output_with_notes() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    fs::write(mirror.path().join("hello.txt"), "hello\n").expect("should write file");
    mirror
        .stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create a note and push it. Use -f in case hooks already created one.
    mirror
        .git_og(&["notes", "--ref=ai", "add", "-f", "-m", "test note", "HEAD"])
        .expect("should add note");
    mirror
        .git_og(&["push", "origin", "refs/notes/ai"])
        .expect("should push notes");
    mirror
        .git_og(&["update-ref", "-d", "refs/notes/ai"])
        .expect("should delete local note ref");

    let output = mirror
        .git_ai(&["fetch-notes"])
        .expect("fetch-notes should succeed");
    assert!(
        output.contains("done"),
        "expected 'done' message, got: {}",
        output
    );
}

#[test]
fn test_fetch_notes_remote_missing_value_fails() {
    let repo = TestRepo::new();

    let err = repo
        .git_ai(&["fetch-notes", "--remote"])
        .expect_err("--remote without value should fail");
    assert!(
        err.contains("--remote requires a value"),
        "error should mention missing value, got: {}",
        err
    );
}

#[test]
fn test_fetch_notes_duplicate_remote_fails() {
    let repo = TestRepo::new();

    let err = repo
        .git_ai(&["fetch-notes", "origin", "--remote", "upstream"])
        .expect_err("duplicate remote should fail");
    assert!(
        err.contains("remote specified more than once"),
        "error should mention duplicate remote, got: {}",
        err
    );
}

#[test]
fn test_fetch_notes_json_error_includes_remote() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    fs::write(mirror.path().join("hello.txt"), "hello\n").expect("should write file");
    mirror
        .stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Remove the origin remote so the fetch fails
    mirror
        .git_og(&["remote", "remove", "origin"])
        .expect("should remove origin");

    let err = mirror
        .git_ai(&["fetch-notes", "--json", "--remote", "nonexistent"])
        .expect_err("fetch from nonexistent remote should fail");

    // Error JSON should include the remote name we passed
    let parsed = extract_json(&err);
    assert_eq!(parsed["status"], "fetch_failed");
    assert_eq!(parsed["remote"], "nonexistent");
    assert!(parsed["error"].as_str().is_some());
}
