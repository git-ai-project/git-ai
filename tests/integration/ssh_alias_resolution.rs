use crate::repos::test_repo::{GitTestMode, TestRepo};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

fn sh_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn set_executable(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn write_capture_hook_script(repo: &TestRepo, name: &str, output_path: &Path) -> String {
    let script_path = repo.path().join(format!("{name}.sh"));
    let quoted_output = sh_single_quote(&output_path.to_string_lossy());

    let script =
        format!("#!/bin/sh\nset -eu\ncat >> {quoted_output}\nprintf '\\n' >> {quoted_output}\n");

    fs::write(&script_path, script).unwrap();
    set_executable(&script_path);

    sh_single_quote(&script_path.to_string_lossy())
}

fn configure_post_notes_updated_hook(repo: &TestRepo, command: &str) {
    repo.git_ai(&["config", "set", "git_ai_hooks.post_notes_updated", command])
        .expect("failed to set post_notes_updated hook command");
}

fn wait_for_json_lines(path: &Path, expected_count: usize, timeout: Duration) -> Vec<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(contents) = fs::read_to_string(path) {
            let lines: Vec<&str> = contents
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            if lines.len() >= expected_count {
                return lines
                    .into_iter()
                    .map(|l| serde_json::from_str::<Value>(l).unwrap())
                    .collect();
            }
        }
        if Instant::now() >= deadline {
            let contents = fs::read_to_string(path).unwrap_or_default();
            panic!(
                "timed out waiting for {} JSON lines in {}. Contents:\n{}",
                expected_count,
                path.display(),
                contents
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn write_ssh_config(repo: &TestRepo, content: &str) {
    let ssh_dir = repo.test_home_path().join(".ssh");
    fs::create_dir_all(&ssh_dir).unwrap();
    fs::write(ssh_dir.join("config"), content).unwrap();
}

fn setup_ai_commit(repo: &TestRepo) -> String {
    let file_path = repo.path().join("main.rs");
    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.git(&["add", "main.rs"]).unwrap();
    repo.commit("base commit").unwrap();

    // Add an AI-authored line and commit
    fs::write(&file_path, "fn main() {}\nfn ai_func() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.rs"]).unwrap();
    repo.git(&["add", "main.rs"]).unwrap();
    repo.commit("ai commit").unwrap().commit_sha
}

#[test]
fn ssh_alias_resolves_in_hook_context() {
    let repo = TestRepo::new_with_mode(GitTestMode::Hooks);

    // Set up SSH config with alias
    write_ssh_config(
        &repo,
        "Host github-work\n  HostName github.com\n  User git\n",
    );

    // Set remote to use the alias
    repo.git(&[
        "remote",
        "add",
        "origin",
        "git@github-work:my-org/my-repo.git",
    ])
    .unwrap();

    // Configure hook to capture payload
    let payload_log = repo.path().join("hook-output.ndjson");
    let hook_cmd = write_capture_hook_script(&repo, "capture-hook", &payload_log);
    configure_post_notes_updated_hook(&repo, &hook_cmd);

    let _commit_sha = setup_ai_commit(&repo);

    let hook_calls = wait_for_json_lines(&payload_log, 1, Duration::from_secs(6));
    let payload = hook_calls[0].as_array().expect("payload should be array");
    assert!(
        !payload.is_empty(),
        "payload should have at least one entry"
    );

    let entry = &payload[0];
    let repo_url = entry
        .get("repo_url")
        .and_then(Value::as_str)
        .expect("repo_url should be present");

    assert_eq!(
        repo_url, "https://github.com/my-org/my-repo",
        "SSH alias 'github-work' should resolve to 'github.com'"
    );

    let repo_name = entry
        .get("repo_name")
        .and_then(Value::as_str)
        .expect("repo_name should be present");
    assert_eq!(
        repo_name, "my-repo",
        "repo_name should be derived from resolved URL"
    );
}

#[test]
fn ssh_alias_no_config_falls_back_to_literal() {
    let repo = TestRepo::new_with_mode(GitTestMode::Hooks);

    // Do NOT write SSH config — the alias should fall back to literal
    repo.git(&["remote", "add", "origin", "git@my-alias:org/repo.git"])
        .unwrap();

    let payload_log = repo.path().join("hook-output.ndjson");
    let hook_cmd = write_capture_hook_script(&repo, "capture-hook", &payload_log);
    configure_post_notes_updated_hook(&repo, &hook_cmd);

    let _commit_sha = setup_ai_commit(&repo);

    let hook_calls = wait_for_json_lines(&payload_log, 1, Duration::from_secs(6));
    let payload = hook_calls[0].as_array().expect("payload should be array");

    let repo_url = payload[0]
        .get("repo_url")
        .and_then(Value::as_str)
        .expect("repo_url should be present");

    assert_eq!(
        repo_url, "https://my-alias/org/repo",
        "without SSH config, alias should be used literally"
    );
}

#[test]
fn ssh_alias_no_matching_host_falls_back() {
    let repo = TestRepo::new_with_mode(GitTestMode::Hooks);

    // SSH config with a different alias — no match for our remote
    write_ssh_config(&repo, "Host other-alias\n  HostName gitlab.com\n");

    repo.git(&["remote", "add", "origin", "git@github-work:org/repo.git"])
        .unwrap();

    let payload_log = repo.path().join("hook-output.ndjson");
    let hook_cmd = write_capture_hook_script(&repo, "capture-hook", &payload_log);
    configure_post_notes_updated_hook(&repo, &hook_cmd);

    let _commit_sha = setup_ai_commit(&repo);

    let hook_calls = wait_for_json_lines(&payload_log, 1, Duration::from_secs(6));
    let payload = hook_calls[0].as_array().expect("payload should be array");

    let repo_url = payload[0]
        .get("repo_url")
        .and_then(Value::as_str)
        .expect("repo_url should be present");

    assert_eq!(
        repo_url, "https://github-work/org/repo",
        "non-matching alias should fall back to literal hostname"
    );
}

#[test]
fn dotted_ssh_alias_resolves() {
    let repo = TestRepo::new_with_mode(GitTestMode::Hooks);

    // SSH config remapping a dotted hostname to another
    write_ssh_config(
        &repo,
        "Host github.com\n  HostName internal-github.corp.example.com\n",
    );

    repo.git(&["remote", "add", "origin", "git@github.com:org/repo.git"])
        .unwrap();

    let payload_log = repo.path().join("hook-output.ndjson");
    let hook_cmd = write_capture_hook_script(&repo, "capture-hook", &payload_log);
    configure_post_notes_updated_hook(&repo, &hook_cmd);

    let _commit_sha = setup_ai_commit(&repo);

    let hook_calls = wait_for_json_lines(&payload_log, 1, Duration::from_secs(6));
    let payload = hook_calls[0].as_array().expect("payload should be array");

    let repo_url = payload[0]
        .get("repo_url")
        .and_then(Value::as_str)
        .expect("repo_url should be present");

    assert_eq!(
        repo_url, "https://internal-github.corp.example.com/org/repo",
        "dotted SSH alias should resolve to HostName from config"
    );
}

#[test]
fn ssh_alias_multiple_hosts_in_config() {
    let repo = TestRepo::new_with_mode(GitTestMode::Hooks);

    write_ssh_config(
        &repo,
        "Host github-work gh-work\n  HostName github.com\n  User git\n\n\
         Host gitlab-work\n  HostName gitlab.com\n",
    );

    // Test with first alias from multi-alias Host line
    repo.git(&["remote", "add", "origin", "git@gh-work:org/repo.git"])
        .unwrap();

    let payload_log = repo.path().join("hook-output.ndjson");
    let hook_cmd = write_capture_hook_script(&repo, "capture-hook", &payload_log);
    configure_post_notes_updated_hook(&repo, &hook_cmd);

    let _commit_sha = setup_ai_commit(&repo);

    let hook_calls = wait_for_json_lines(&payload_log, 1, Duration::from_secs(6));
    let payload = hook_calls[0].as_array().expect("payload should be array");

    let repo_url = payload[0]
        .get("repo_url")
        .and_then(Value::as_str)
        .expect("repo_url should be present");

    assert_eq!(
        repo_url, "https://github.com/org/repo",
        "multi-alias Host line should resolve each alias correctly"
    );
}
