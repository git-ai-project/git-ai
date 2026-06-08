use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{TestRepo, get_binary_path};
use git_ai::authorship::stats::CommitStats;
use std::fs;
use std::process::{Command, Output};

fn extract_json_object(output: &str) -> String {
    let start = output.find('{').expect("output should contain JSON");
    let end = output.rfind('}').expect("output should contain JSON");
    output[start..=end].to_string()
}

fn run_git_ai_raw(repo: &TestRepo, args: &[&str]) -> Output {
    let mut command = Command::new(get_binary_path());
    command.args(args).current_dir(repo.path());

    command.env("HOME", repo.test_home_path());
    command.env(
        "GIT_CONFIG_GLOBAL",
        repo.test_home_path().join(".gitconfig"),
    );
    command.env("XDG_CONFIG_HOME", repo.test_home_path().join(".config"));
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env("GIT_AI_DAEMON_HOME", repo.daemon_home_path());
    command.env(
        "GIT_AI_DAEMON_CONTROL_SOCKET",
        repo.daemon_control_socket_path(),
    );
    command.env(
        "GIT_AI_DAEMON_TRACE_SOCKET",
        repo.daemon_trace_socket_path(),
    );
    command.env("GIT_AI_TEST_DB_PATH", repo.test_db_path());
    command.env("GITAI_TEST_DB_PATH", repo.test_db_path());
    if let Some(patch_json) = repo.config_patch_json() {
        command.env("GIT_AI_TEST_CONFIG_PATCH", patch_json);
    }

    command.output().expect("git-ai command should run")
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn await_stats_prints_json_when_note_exists() {
    let repo = TestRepo::new();
    let mut file = repo.filename("await.txt");
    file.set_contents(crate::lines!["AI one".ai(), "AI two".ai()]);
    repo.stage_all_and_commit("Add AI lines").unwrap();

    let raw = repo
        .git_ai(&[
            "await-stats",
            "--timeout",
            "1000",
            "--interval",
            "10",
            "--json",
        ])
        .expect("await-stats should succeed");
    let stats: CommitStats = serde_json::from_str(&extract_json_object(&raw)).unwrap();

    assert_eq!(stats.ai_additions, 2);
    assert_eq!(stats.ai_accepted, 2);
    assert_eq!(stats.git_diff_added_lines, 2);
}

#[test]
fn await_stats_defaults_to_head() {
    let repo = TestRepo::new();
    let mut file = repo.filename("head.txt");
    file.set_contents(crate::lines!["AI head".ai()]);
    repo.stage_all_and_commit("Add HEAD line").unwrap();

    let output = repo
        .git_ai(&["await-stats", "--timeout", "1000", "--interval", "10"])
        .expect("await-stats should succeed");

    assert!(output.contains("you"), "output was:\n{}", output);
    assert!(output.contains("ai"), "output was:\n{}", output);
}

#[test]
fn await_stats_accepts_commit_rev() {
    let repo = TestRepo::new();
    let mut file = repo.filename("revs.txt");

    file.set_contents(crate::lines!["First AI".ai()]);
    let first = repo.stage_all_and_commit("First").unwrap();

    file.set_contents(crate::lines!["First AI".ai(), "Second AI".ai()]);
    repo.stage_all_and_commit("Second").unwrap();

    let raw = repo
        .git_ai(&[
            "await-stats",
            "--commit",
            &first.commit_sha,
            "--timeout",
            "1000",
            "--interval",
            "10",
            "--json",
        ])
        .expect("await-stats should succeed for explicit commit");
    let stats: CommitStats = serde_json::from_str(&extract_json_object(&raw)).unwrap();

    assert_eq!(stats.ai_additions, 1);
    assert_eq!(stats.git_diff_added_lines, 1);
}

#[test]
fn await_stats_times_out_when_note_absent() {
    let repo = TestRepo::new();
    let mut file = repo.filename("missing-note.txt");
    file.set_contents(crate::lines!["AI line".ai()]);
    let commit = repo.stage_all_and_commit("Create note").unwrap();

    repo.git_og(&["notes", "--ref=ai", "remove", &commit.commit_sha])
        .expect("removing note should succeed");

    let output = run_git_ai_raw(
        &repo,
        &[
            "await-stats",
            "--timeout",
            "0",
            "--interval",
            "10",
            "--commit",
            &commit.commit_sha,
        ],
    );

    assert_eq!(output.status.code(), Some(1), "{}", output_text(&output));
    assert!(
        output_text(&output).contains("timed out waiting for authorship note"),
        "{}",
        output_text(&output)
    );
}

#[test]
fn await_stats_quiet_suppresses_timeout_output() {
    let repo = TestRepo::new();
    let mut file = repo.filename("quiet-missing-note.txt");
    file.set_contents(crate::lines!["AI line".ai()]);
    let commit = repo.stage_all_and_commit("Create quiet note").unwrap();

    repo.git_og(&["notes", "--ref=ai", "remove", &commit.commit_sha])
        .expect("removing note should succeed");

    let output = run_git_ai_raw(
        &repo,
        &[
            "await-stats",
            "--timeout",
            "0",
            "--interval",
            "10",
            "--commit",
            &commit.commit_sha,
            "--quiet",
        ],
    );

    assert_eq!(output.status.code(), Some(1), "{}", output_text(&output));
    assert_eq!(output_text(&output), "");
}

#[test]
fn await_stats_bad_commit_exits_2() {
    let repo = TestRepo::new();
    let output = run_git_ai_raw(
        &repo,
        &["await-stats", "--commit", "definitely-not-a-commit"],
    );

    assert_eq!(output.status.code(), Some(2), "{}", output_text(&output));
    assert!(
        output_text(&output).contains("No commit found: definitely-not-a-commit"),
        "{}",
        output_text(&output)
    );
}

#[test]
fn await_stats_corrupt_note_exits_3() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("corrupt.txt");
    fs::write(&file_path, "AI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "corrupt.txt"])
        .unwrap();
    let commit = repo
        .stage_all_and_commit("Create corruptible note")
        .unwrap();

    repo.git_og(&[
        "notes",
        "--ref=ai",
        "add",
        "-f",
        "-m",
        "not json",
        &commit.commit_sha,
    ])
    .expect("overwriting note should succeed");

    let output = run_git_ai_raw(
        &repo,
        &[
            "await-stats",
            "--commit",
            &commit.commit_sha,
            "--timeout",
            "0",
            "--interval",
            "10",
        ],
    );

    assert_eq!(output.status.code(), Some(3), "{}", output_text(&output));
    assert!(
        output_text(&output).contains("Failed to read authorship note"),
        "{}",
        output_text(&output)
    );
}
