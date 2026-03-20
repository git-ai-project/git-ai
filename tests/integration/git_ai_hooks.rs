use crate::repos::test_repo::{GitTestMode, TestRepo};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

fn sh_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn set_executable(path: &Path) {
    let mut perms = fs::metadata(path)
        .expect("hook script metadata should exist")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("failed to set executable bit on hook script");
}

fn write_capture_hook_script(
    repo: &TestRepo,
    name: &str,
    output_path: &Path,
    append_newline_per_invocation: bool,
    delay_seconds: Option<u64>,
) -> String {
    let script_path = repo.path().join(format!("{name}.sh"));
    let quoted_output = sh_single_quote(&output_path.to_string_lossy());

    let mut script = String::from("#!/bin/sh\nset -eu\n");
    if let Some(delay) = delay_seconds {
        script.push_str(&format!("sleep {}\n", delay));
    }

    if append_newline_per_invocation {
        script.push_str(&format!(
            "cat >> {quoted_output}\nprintf '\\n' >> {quoted_output}\n"
        ));
    } else {
        script.push_str(&format!("cat > {quoted_output}\n"));
    }

    fs::write(&script_path, script).expect("failed to write hook capture script");
    set_executable(&script_path);

    sh_single_quote(&script_path.to_string_lossy())
}

fn configure_post_notes_updated_hooks(repo: &TestRepo, commands: &[String]) {
    assert!(
        !commands.is_empty(),
        "at least one hook command is required"
    );

    repo.git_ai(&[
        "config",
        "set",
        "git_ai_hooks.post_notes_updated",
        commands[0].as_str(),
    ])
    .expect("failed to set first post_notes_updated hook command");

    for command in &commands[1..] {
        repo.git_ai(&[
            "config",
            "--add",
            "git_ai_hooks.post_notes_updated",
            command.as_str(),
        ])
        .expect("failed to add additional post_notes_updated hook command");
    }
}

fn wait_for_json_lines(path: &Path, expected_line_count: usize, timeout: Duration) -> Vec<Value> {
    let deadline = Instant::now() + timeout;

    loop {
        if let Ok(contents) = fs::read_to_string(path) {
            let lines: Vec<&str> = contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect();

            if lines.len() >= expected_line_count {
                let parsed = lines
                    .into_iter()
                    .map(|line| {
                        serde_json::from_str::<Value>(line).unwrap_or_else(|e| {
                            panic!("invalid hook payload JSON line '{}': {}", line, e)
                        })
                    })
                    .collect::<Vec<_>>();
                return parsed;
            }
        }

        if Instant::now() >= deadline {
            let contents = fs::read_to_string(path).unwrap_or_default();
            panic!(
                "timed out waiting for {} JSON hook lines in {}. Current contents:\n{}",
                expected_line_count,
                path.display(),
                contents
            );
        }

        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_json_file(path: &Path, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;

    loop {
        if let Ok(contents) = fs::read_to_string(path)
            && !contents.trim().is_empty()
        {
            return serde_json::from_str::<Value>(&contents).unwrap_or_else(|e| {
                panic!(
                    "invalid hook payload JSON file {}: {}\n{}",
                    path.display(),
                    e,
                    contents
                )
            });
        }

        if Instant::now() >= deadline {
            let contents = fs::read_to_string(path).unwrap_or_default();
            panic!(
                "timed out waiting for JSON hook payload file {}. Current contents:\n{}",
                path.display(),
                contents
            );
        }

        std::thread::sleep(Duration::from_millis(25));
    }
}

fn append_ai_line_and_commit(
    repo: &TestRepo,
    file_name: &str,
    line: &str,
    message: &str,
) -> String {
    let file_path = repo.path().join(file_name);
    let mut content = fs::read_to_string(&file_path).expect("file to append should exist");
    content.push_str(line);
    if !line.ends_with('\n') {
        content.push('\n');
    }
    fs::write(&file_path, content).expect("failed to write updated file content");

    repo.git_ai(&["checkpoint", "mock_ai", file_name])
        .expect("checkpoint should succeed");
    repo.git(&["add", file_name])
        .expect("staging should succeed");
    repo.commit(message)
        .expect("ai commit should succeed")
        .commit_sha
}

fn expected_hook_keys() -> HashSet<&'static str> {
    [
        "commit_sha",
        "repo_url",
        "repo_name",
        "branch",
        "is_default_branch",
        "note_content",
    ]
    .into_iter()
    .collect()
}

fn assert_note_entry_payload(
    repo: &TestRepo,
    entry: &Value,
    expected_commit_sha: &str,
    expected_branch: &str,
    expected_is_default_branch: bool,
) {
    let obj = entry
        .as_object()
        .expect("hook entry should be a JSON object");
    let keys: HashSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        expected_hook_keys(),
        "hook entry should expose only snake_case post_notes_updated fields"
    );

    assert_eq!(
        entry.get("commit_sha").and_then(Value::as_str),
        Some(expected_commit_sha),
        "hook entry commit_sha should match"
    );
    assert_eq!(
        entry.get("repo_url").and_then(Value::as_str),
        Some(""),
        "local test repos should have empty repo_url"
    );
    assert_eq!(
        entry.get("repo_name").and_then(Value::as_str),
        Some(""),
        "local test repos should have empty repo_name"
    );
    assert_eq!(
        entry.get("branch").and_then(Value::as_str),
        Some(expected_branch),
        "hook entry branch should match"
    );
    assert_eq!(
        entry.get("is_default_branch").and_then(Value::as_bool),
        Some(expected_is_default_branch),
        "hook entry is_default_branch should match"
    );

    let note_from_git = repo
        .git(&["notes", "--ref=ai", "show", expected_commit_sha])
        .expect("notes show should succeed");
    let note_content = entry
        .get("note_content")
        .and_then(Value::as_str)
        .expect("hook entry note_content should be a string");
    assert_eq!(
        note_content.trim_end_matches('\n'),
        note_from_git.trim_end_matches('\n'),
        "hook entry note_content should match the exact note stored in refs/notes/ai"
    );
    assert!(
        !note_content.trim().is_empty(),
        "hook entry note_content should be non-empty"
    );
}

#[test]
fn post_notes_updated_hook_sends_single_note_array_via_stdin() {
    let repo = TestRepo::new_with_mode(GitTestMode::Hooks);

    let payload_log = repo.path().join("post-notes-single.ndjson");

    let file_path = repo.path().join("story.txt");
    fs::write(&file_path, "base line\n").expect("failed to write base file");
    repo.git(&["add", "story.txt"])
        .expect("staging base file should succeed");
    repo.commit("base commit")
        .expect("base commit should succeed");

    let hook_command =
        write_capture_hook_script(&repo, "capture-post-notes-single", &payload_log, true, None);
    configure_post_notes_updated_hooks(&repo, &[hook_command]);

    let commit_sha = append_ai_line_and_commit(&repo, "story.txt", "ai line", "ai commit");

    let hook_calls = wait_for_json_lines(&payload_log, 1, Duration::from_secs(6));
    assert_eq!(
        hook_calls.len(),
        1,
        "single commit path should invoke post_notes_updated exactly once"
    );

    let payload = hook_calls[0]
        .as_array()
        .expect("post_notes_updated payload should be a JSON array");
    assert_eq!(
        payload.len(),
        1,
        "single commit path should emit a 1-item note array"
    );

    assert_note_entry_payload(&repo, &payload[0], &commit_sha, "main", true);
}

#[test]
fn post_notes_updated_hook_batches_rebase_notes_into_single_payload() {
    let repo = TestRepo::new_with_mode(GitTestMode::Hooks);

    let payload_log = repo.path().join("post-notes-rebase.ndjson");

    let file_name = "rebase-batch.txt";
    let file_path = repo.path().join(file_name);

    fs::write(&file_path, "base\n").expect("failed to write base file");
    repo.git(&["add", file_name])
        .expect("staging base file should succeed");
    repo.commit("base commit")
        .expect("base commit should succeed");

    repo.git(&["checkout", "-b", "feature"])
        .expect("feature branch creation should succeed");
    append_ai_line_and_commit(&repo, file_name, "feature ai 1", "feature ai commit 1");
    append_ai_line_and_commit(&repo, file_name, "feature ai 2", "feature ai commit 2");

    repo.git(&["checkout", "main"])
        .expect("checkout main should succeed");
    let main_only_file = repo.path().join("main-only.txt");
    fs::write(&main_only_file, "main line\n").expect("failed to write main-only file");
    repo.git(&["add", "main-only.txt"])
        .expect("staging main-only file should succeed");
    repo.commit("main update")
        .expect("main update commit should succeed");

    let hook_command =
        write_capture_hook_script(&repo, "capture-post-notes-rebase", &payload_log, true, None);
    configure_post_notes_updated_hooks(&repo, &[hook_command]);
    fs::write(&payload_log, "").expect("failed to clear payload log before rebase assertions");

    repo.git(&["checkout", "feature"])
        .expect("checkout feature should succeed");
    repo.git(&["rebase", "main"])
        .expect("rebase should succeed");

    let hook_calls = wait_for_json_lines(&payload_log, 1, Duration::from_secs(8));
    assert_eq!(
        hook_calls.len(),
        1,
        "rebase note rewrite should invoke post_notes_updated once with a batch payload"
    );

    let payload = hook_calls[0]
        .as_array()
        .expect("post_notes_updated payload should be a JSON array");
    assert_eq!(
        payload.len(),
        2,
        "rebase rewrite should emit both remapped notes in one payload"
    );

    let expected_commits: Vec<String> = repo
        .git(&["rev-list", "--reverse", "main..HEAD"])
        .expect("rev-list should succeed")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    assert_eq!(
        expected_commits.len(),
        2,
        "feature branch should contain two rebased commits"
    );

    let payload_commits: Vec<&str> = payload
        .iter()
        .map(|entry| {
            entry
                .get("commit_sha")
                .and_then(Value::as_str)
                .expect("payload commit_sha should be a string")
        })
        .collect();

    assert_eq!(
        payload_commits,
        expected_commits
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        "batched hook payload should preserve commit order"
    );

    for (entry, commit_sha) in payload.iter().zip(expected_commits.iter()) {
        assert_note_entry_payload(&repo, entry, commit_sha, "feature", false);
    }
}

#[test]
fn post_notes_updated_hooks_run_in_parallel_and_detach_after_timeout() {
    let repo = TestRepo::new_with_mode(GitTestMode::Hooks);

    let payload_fast = repo.path().join("post-notes-timeout-a.json");
    let payload_slow = repo.path().join("post-notes-timeout-b.json");

    let file_name = "timeout-hooks.txt";
    fs::write(repo.path().join(file_name), "base\n").expect("failed to write base file");
    repo.git(&["add", file_name])
        .expect("staging base file should succeed");
    repo.commit("base commit")
        .expect("base commit should succeed");

    let hook_a = write_capture_hook_script(
        &repo,
        "capture-post-notes-timeout-a",
        &payload_fast,
        false,
        Some(5),
    );
    let hook_b = write_capture_hook_script(
        &repo,
        "capture-post-notes-timeout-b",
        &payload_slow,
        false,
        Some(5),
    );
    configure_post_notes_updated_hooks(&repo, &[hook_a, hook_b]);

    let file_path = repo.path().join(file_name);
    let mut content = fs::read_to_string(&file_path).expect("failed to read timeout file");
    content.push_str("ai line\n");
    fs::write(&file_path, content).expect("failed to append ai line");

    repo.git_ai(&["checkpoint", "mock_ai", file_name])
        .expect("checkpoint should succeed");
    repo.git(&["add", file_name])
        .expect("staging timeout file should succeed");

    let start = Instant::now();
    let commit_sha = repo
        .commit("ai commit with slow hooks")
        .expect("ai commit should succeed")
        .commit_sha;
    let elapsed = start.elapsed();

    assert!(
        elapsed >= Duration::from_millis(2500),
        "git-ai should wait roughly the configured timeout before detaching (elapsed: {:?})",
        elapsed
    );
    assert!(
        elapsed < Duration::from_millis(8500),
        "git-ai should detach rather than waiting for both 5s hooks to fully finish (elapsed: {:?})",
        elapsed
    );

    let payload_a = wait_for_json_file(&payload_fast, Duration::from_secs(12));
    let payload_b = wait_for_json_file(&payload_slow, Duration::from_secs(12));

    let entries_a = payload_a
        .as_array()
        .expect("hook A payload should be a JSON array");
    let entries_b = payload_b
        .as_array()
        .expect("hook B payload should be a JSON array");

    assert_eq!(entries_a.len(), 1, "hook A should receive one note entry");
    assert_eq!(entries_b.len(), 1, "hook B should receive one note entry");

    assert_note_entry_payload(&repo, &entries_a[0], &commit_sha, "main", true);
    assert_note_entry_payload(&repo, &entries_b[0], &commit_sha, "main", true);
}

#[test]
fn git_ai_hooks_config_cli_supports_set_add_get_and_unset_nested_hooks() {
    let repo = TestRepo::new_with_mode(GitTestMode::Hooks);

    let cmd_a = "echo hook-a";
    let cmd_b = "echo hook-b";

    repo.git_ai(&["config", "set", "git_ai_hooks.post_notes_updated", cmd_a])
        .expect("setting first post_notes_updated command should succeed");
    repo.git_ai(&["config", "--add", "git_ai_hooks.post_notes_updated", cmd_b])
        .expect("adding second post_notes_updated command should succeed");

    let hook_values = repo
        .git_ai(&["config", "git_ai_hooks.post_notes_updated"])
        .expect("config get for nested git_ai_hooks key should succeed");
    let parsed_values: Vec<String> =
        serde_json::from_str(&hook_values).expect("nested hook get output should be valid JSON");
    assert_eq!(
        parsed_values,
        vec![cmd_a.to_string(), cmd_b.to_string()],
        "nested hook value should preserve both configured commands"
    );

    repo.git_ai(&[
        "config",
        "set",
        "git_ai_hooks",
        r#"{"post_notes_updated":["echo one"],"future_hook":["echo future"]}"#,
    ])
    .expect("setting top-level git_ai_hooks object should succeed");

    let all_hooks = repo
        .git_ai(&["config", "git_ai_hooks"])
        .expect("config get for git_ai_hooks should succeed");
    let parsed_hooks: serde_json::Value =
        serde_json::from_str(&all_hooks).expect("git_ai_hooks output should be valid JSON");
    assert_eq!(
        parsed_hooks["post_notes_updated"],
        serde_json::json!(["echo one"]),
        "top-level git_ai_hooks set should update existing hook map"
    );
    assert_eq!(
        parsed_hooks["future_hook"],
        serde_json::json!(["echo future"]),
        "top-level git_ai_hooks set should support future hooks"
    );

    repo.git_ai(&["config", "unset", "git_ai_hooks.future_hook"])
        .expect("unsetting nested future hook should succeed");

    let remaining_hooks = repo
        .git_ai(&["config", "git_ai_hooks"])
        .expect("config get for git_ai_hooks after unset should succeed");
    let remaining_hooks_json: serde_json::Value =
        serde_json::from_str(&remaining_hooks).expect("remaining hooks should be valid JSON");
    assert!(
        remaining_hooks_json.get("future_hook").is_none(),
        "unset should remove only the targeted nested hook"
    );
    assert_eq!(
        remaining_hooks_json["post_notes_updated"],
        serde_json::json!(["echo one"]),
        "unset should not remove unrelated nested hooks"
    );
}
