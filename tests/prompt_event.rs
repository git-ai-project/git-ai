//! Integration tests for the prompt-event command.
//!
//! These tests verify that the prompt-event command correctly:
//! 1. Parses Claude Code JSONL transcripts
//! 2. Computes stable, content-based event IDs
//! 3. Tracks parent relationships between events
//! 4. Handles rollbacks/session resets
//! 5. Only runs in async_mode with daemon

#[macro_use]
#[path = "integration/repos/mod.rs"]
mod repos;

use git_ai::daemon::{
    ControlRequest, local_socket_connects_with_timeout, send_control_request,
};
use repos::test_repo::{GitTestMode, TestRepo, get_binary_path, real_git_executable};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

const DAEMON_TEST_PROBE_TIMEOUT: Duration = Duration::from_millis(100);

fn configure_test_home_env(command: &mut Command, repo: &TestRepo) {
    command.env("HOME", repo.test_home_path());
    command.env(
        "GIT_CONFIG_GLOBAL",
        repo.test_home_path().join(".gitconfig"),
    );
}

fn configure_test_daemon_env(command: &mut Command, repo: &TestRepo) {
    command.env("GIT_AI_DAEMON_HOME", repo.daemon_home_path());
    command.env(
        "GIT_AI_DAEMON_CONTROL_SOCKET",
        repo.daemon_control_socket_path(),
    );
    command.env(
        "GIT_AI_DAEMON_TRACE_SOCKET",
        repo.daemon_trace_socket_path(),
    );
}

fn write_async_mode_config(repo: &TestRepo) {
    let config_dir = repo.test_home_path().join(".git-ai");
    fs::create_dir_all(&config_dir).expect("failed to create async mode config dir");
    let config_path = config_dir.join("config.json");
    let config = serde_json::json!({
        "git_path": real_git_executable(),
        "disable_auto_updates": true,
        "feature_flags": {
            "async_mode": true,
            "git_hooks_enabled": false
        },
        "quiet": false
    });
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("failed to serialize config"),
    )
    .expect("failed to write config");
}

fn write_transcript(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("failed to create transcript dir");
    }
    fs::write(path, content).expect("failed to write transcript");
}

fn run_prompt_event(repo: &TestRepo, agent: &str, hook_input: &str) -> (bool, String, String) {
    let binary_path = get_binary_path();
    let mut command = Command::new(binary_path);
    command.args(["prompt-event", agent, "--hook-input", "stdin"]);
    command.current_dir(repo.path());
    configure_test_home_env(&mut command, repo);
    configure_test_daemon_env(&mut command, repo);
    command.env("GIT_AI_TEST_DB_PATH", repo.test_db_path());

    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let mut child = command.spawn().expect("failed to start prompt-event");
    if let Some(stdin) = child.stdin.take() {
        use std::io::Write;
        let mut stdin = stdin;
        stdin
            .write_all(hook_input.as_bytes())
            .expect("failed to write hook input");
    }

    let output = child.wait_with_output().expect("failed to wait for prompt-event");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status.success(), stdout, stderr)
}

fn read_prompt_event_state(repo: &TestRepo, session_id: &str) -> Option<serde_json::Value> {
    let state_path = repo
        .test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("prompt-events")
        .join(format!("{}.json", session_id));
    if state_path.exists() {
        let content = fs::read_to_string(&state_path).ok()?;
        serde_json::from_str(&content).ok()
    } else {
        None
    }
}

fn daemon_command_output(
    repo: &TestRepo,
    args: &[&str],
    cwd: &Path,
) -> std::process::Output {
    let mut command = Command::new(get_binary_path());
    command.args(args).current_dir(cwd);
    configure_test_home_env(&mut command, repo);
    configure_test_daemon_env(&mut command, repo);
    command
        .output()
        .expect("failed to invoke git-ai daemon command")
}

fn wait_for_daemon_sockets(repo: &TestRepo) {
    let control = repo.daemon_control_socket_path();
    let trace = repo.daemon_trace_socket_path();
    for _ in 0..200 {
        let status = send_control_request(
            &control,
            &ControlRequest::StatusFamily {
                repo_working_dir: repo.canonical_path().to_string_lossy().to_string(),
            },
        );
        if status.is_ok()
            && local_socket_connects_with_timeout(&trace, DAEMON_TEST_PROBE_TIMEOUT).is_ok()
        {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("daemon sockets did not become ready");
}

fn shutdown_daemon(repo: &TestRepo) {
    let output = daemon_command_output(repo, &["bg", "shutdown"], repo.test_home_path());
    assert!(
        output.status.success(),
        "daemon shutdown should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// --- Tests ---

#[test]
fn prompt_event_skips_without_async_mode() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    // Don't write async_mode config - command should silently skip
    let transcript_path = repo.path().join("transcript.jsonl");
    write_transcript(
        &transcript_path,
        r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"Hello"}}"#,
    );

    let hook_input = serde_json::json!({
        "transcript_path": transcript_path.to_str().unwrap(),
        "session_id": "test-session",
        "hook_event_name": "UserPromptSubmit",
        "cwd": repo.path().to_str().unwrap(),
    });

    let (success, _stdout, stderr) = run_prompt_event(&repo, "claude", &hook_input.to_string());
    assert!(success, "prompt-event should exit 0 when async_mode is off: {}", stderr);
}

#[test]
fn prompt_event_processes_claude_transcript_with_daemon() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    write_async_mode_config(&repo);

    // Start daemon
    let start_output = daemon_command_output(&repo, &["bg", "start"], repo.path());
    assert!(
        start_output.status.success(),
        "daemon start should succeed: stderr={}",
        String::from_utf8_lossy(&start_output.stderr)
    );
    wait_for_daemon_sockets(&repo);

    // Create transcript
    let transcript_dir = repo.test_home_path().join(".claude").join("projects").join("test");
    let transcript_path = transcript_dir.join("test-session-abc.jsonl");
    write_transcript(
        &transcript_path,
        r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"Fix the bug"}}
{"type":"assistant","timestamp":"2026-01-01T00:00:01Z","message":{"model":"claude-3","content":[{"type":"thinking","thinking":"Let me analyze the issue"},{"type":"text","text":"I found the bug"},{"type":"tool_use","id":"toolu_123","name":"Edit","input":{"file_path":"src/main.rs"}}]}}"#,
    );

    let hook_input = serde_json::json!({
        "transcript_path": transcript_path.to_str().unwrap(),
        "session_id": "test-session-abc",
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "cwd": repo.path().to_str().unwrap(),
    });

    let (success, _stdout, stderr) = run_prompt_event(&repo, "claude", &hook_input.to_string());
    assert!(
        success,
        "prompt-event should succeed with daemon: {}",
        stderr
    );

    // Check state file was created
    let state = read_prompt_event_state(&repo, "test-session-abc");
    assert!(state.is_some(), "state file should exist after prompt-event");

    let state = state.unwrap();
    let emitted = state["emitted_event_ids"].as_array().unwrap();
    assert_eq!(
        emitted.len(),
        4,
        "should have emitted 4 events (user + thinking + text + tool_use): {:?}",
        emitted
    );

    // Verify event IDs are prefixed correctly
    for eid in emitted {
        let id_str = eid.as_str().unwrap();
        assert!(
            id_str.contains(":"),
            "event ID should be prefixed with prompt_id: {}",
            id_str
        );
    }

    // Verify last_line_count
    assert_eq!(
        state["last_line_count"].as_u64().unwrap(),
        4,
        "last_line_count should track total transcript events"
    );

    shutdown_daemon(&repo);
}

#[test]
fn prompt_event_idempotent_rerun() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    write_async_mode_config(&repo);

    // Start daemon
    let start_output = daemon_command_output(&repo, &["bg", "start"], repo.path());
    assert!(start_output.status.success());
    wait_for_daemon_sockets(&repo);

    let transcript_path = repo.test_home_path().join("transcript.jsonl");
    write_transcript(
        &transcript_path,
        r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"Hello"}}"#,
    );

    let hook_input = serde_json::json!({
        "transcript_path": transcript_path.to_str().unwrap(),
        "session_id": "idempotent-test",
        "hook_event_name": "UserPromptSubmit",
        "cwd": repo.path().to_str().unwrap(),
    });

    // Run twice
    let (s1, _, _) = run_prompt_event(&repo, "claude", &hook_input.to_string());
    assert!(s1);
    let state1 = read_prompt_event_state(&repo, "idempotent-test").unwrap();

    let (s2, _, _) = run_prompt_event(&repo, "claude", &hook_input.to_string());
    assert!(s2);
    let state2 = read_prompt_event_state(&repo, "idempotent-test").unwrap();

    // State should be identical - no new events on rerun
    assert_eq!(
        state1["emitted_event_ids"].as_array().unwrap().len(),
        state2["emitted_event_ids"].as_array().unwrap().len(),
        "rerun should not emit duplicate events"
    );

    shutdown_daemon(&repo);
}

#[test]
fn prompt_event_handles_transcript_growth() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    write_async_mode_config(&repo);

    let start_output = daemon_command_output(&repo, &["bg", "start"], repo.path());
    assert!(start_output.status.success());
    wait_for_daemon_sockets(&repo);

    let transcript_path = repo.test_home_path().join("growth-transcript.jsonl");

    // First run with 1 message
    write_transcript(
        &transcript_path,
        r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"Hello"}}"#,
    );

    let hook_input = serde_json::json!({
        "transcript_path": transcript_path.to_str().unwrap(),
        "session_id": "growth-test",
        "hook_event_name": "UserPromptSubmit",
        "cwd": repo.path().to_str().unwrap(),
    });

    let (s1, _, _) = run_prompt_event(&repo, "claude", &hook_input.to_string());
    assert!(s1);
    let state1 = read_prompt_event_state(&repo, "growth-test").unwrap();
    assert_eq!(state1["emitted_event_ids"].as_array().unwrap().len(), 1);

    // Add more messages to transcript
    write_transcript(
        &transcript_path,
        r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"Hello"}}
{"type":"assistant","timestamp":"2026-01-01T00:00:01Z","message":{"model":"claude-3","content":[{"type":"text","text":"Hi there!"}]}}"#,
    );

    let (s2, _, _) = run_prompt_event(&repo, "claude", &hook_input.to_string());
    assert!(s2);
    let state2 = read_prompt_event_state(&repo, "growth-test").unwrap();
    assert_eq!(
        state2["emitted_event_ids"].as_array().unwrap().len(),
        2,
        "should have emitted 1 new event for the assistant message"
    );

    shutdown_daemon(&repo);
}

#[test]
fn prompt_event_handles_rollback() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    write_async_mode_config(&repo);

    let start_output = daemon_command_output(&repo, &["bg", "start"], repo.path());
    assert!(start_output.status.success());
    wait_for_daemon_sockets(&repo);

    let transcript_path = repo.test_home_path().join("rollback-transcript.jsonl");

    // Initial transcript with 3 events
    write_transcript(
        &transcript_path,
        r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"Hello"}}
{"type":"assistant","timestamp":"2026-01-01T00:00:01Z","message":{"model":"claude-3","content":[{"type":"text","text":"Hi!"}]}}
{"type":"user","timestamp":"2026-01-01T00:00:02Z","message":{"content":"Another message"}}"#,
    );

    let hook_input = serde_json::json!({
        "transcript_path": transcript_path.to_str().unwrap(),
        "session_id": "rollback-test",
        "hook_event_name": "Stop",
        "cwd": repo.path().to_str().unwrap(),
    });

    let (s1, _, _) = run_prompt_event(&repo, "claude", &hook_input.to_string());
    assert!(s1);
    let state1 = read_prompt_event_state(&repo, "rollback-test").unwrap();
    assert_eq!(state1["emitted_event_ids"].as_array().unwrap().len(), 3);
    assert_eq!(state1["last_line_count"].as_u64().unwrap(), 3);

    // Simulate rollback - transcript shortened
    write_transcript(
        &transcript_path,
        r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"Hello"}}
{"type":"assistant","timestamp":"2026-01-01T00:00:03Z","message":{"model":"claude-3","content":[{"type":"text","text":"Different response after rollback"}]}}"#,
    );

    let (s2, _, _) = run_prompt_event(&repo, "claude", &hook_input.to_string());
    assert!(s2);
    let state2 = read_prompt_event_state(&repo, "rollback-test").unwrap();

    // After rollback, should have original events + new ones
    let emitted = state2["emitted_event_ids"].as_array().unwrap();
    assert!(
        emitted.len() > 3,
        "should have emitted new events after rollback: got {}",
        emitted.len()
    );

    shutdown_daemon(&repo);
}
