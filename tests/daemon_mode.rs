#[macro_use]
#[path = "integration/repos/mod.rs"]
mod repos;

use git_ai::authorship::working_log::CheckpointKind;
use git_ai::authorship::{transcript::AiTranscript, working_log::AgentId};
use git_ai::commands::checkpoint::{
    PreparedCheckpointFile, PreparedCheckpointFileSource, PreparedCheckpointManifest,
    PreparedPathRole,
};
use git_ai::commands::checkpoint_agent::agent_presets::AgentRunResult;
use git_ai::daemon::{
    CapturedCheckpointRunRequest, CheckpointRunRequest,
    ControlRequest, DaemonConfig, DaemonLock, local_socket_connects_with_timeout,
    open_local_socket_stream_with_timeout, send_control_request,
};
use git_ai::git::find_repository_in_path;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::{
    DaemonTestCompletionLogEntry, DaemonTestScope, GitTestMode, TestRepo, get_binary_path,
    real_git_executable,
};
use serde_json::Value;
use serial_test::serial;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAEMON_TEST_PROBE_TIMEOUT: Duration = Duration::from_millis(100);

fn repo_storage(repo: &TestRepo) -> git_ai::git::repository::Repository {
    find_repository_in_path(repo.path().to_str().expect("repo path should be utf-8"))
        .expect("failed to find repository for daemon test")
}

fn current_head_sha(repo: &TestRepo) -> String {
    repo.git(&["rev-parse", "HEAD"])
        .expect("failed to resolve HEAD")
        .trim()
        .to_string()
}

fn git_common_dir(repo: &TestRepo) -> PathBuf {
    let common_dir = PathBuf::from(
        repo.git(&["rev-parse", "--git-common-dir"])
            .expect("failed to resolve git common dir")
            .trim(),
    );
    if common_dir.is_absolute() {
        common_dir
    } else {
        repo.path().join(common_dir)
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("failed to create destination directory");
    for entry in fs::read_dir(src).expect("failed to read source directory") {
        let entry = entry.expect("failed to read directory entry");
        let dest = dst.join(entry.file_name());
        let file_type = entry.file_type().expect("failed to read file type");
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).expect("failed to copy file");
        }
    }
}

fn daemon_control_socket_path(repo: &TestRepo) -> PathBuf {
    repo.daemon_control_socket_path()
}

fn daemon_trace_socket_path(repo: &TestRepo) -> PathBuf {
    repo.daemon_trace_socket_path()
}

fn daemon_lock_path(repo: &TestRepo) -> PathBuf {
    DaemonConfig::from_home(&repo.daemon_home_path()).lock_path
}

fn send_trace_frames(trace_socket_path: &Path, payloads: &[Value]) {
    let mut stream =
        open_local_socket_stream_with_timeout(trace_socket_path, DAEMON_TEST_PROBE_TIMEOUT)
            .expect("failed to connect to trace socket");
    for payload in payloads {
        let raw = serde_json::to_string(payload).expect("failed to serialize trace payload");
        stream
            .write_all(raw.as_bytes())
            .expect("failed to write trace payload");
        stream
            .write_all(b"\n")
            .expect("failed to write trace newline");
    }
    stream.flush().expect("failed to flush trace payloads");
}

fn repo_workdir_string(repo: &TestRepo) -> String {
    repo.path().to_string_lossy().to_string()
}

fn configure_test_home_env(command: &mut Command, test_home: &Path) {
    command.env("HOME", test_home);
    command.env("GIT_CONFIG_GLOBAL", test_home.join(".gitconfig"));
    #[cfg(windows)]
    {
        command.env("USERPROFILE", test_home);
        command.env("APPDATA", test_home.join("AppData").join("Roaming"));
        command.env("LOCALAPPDATA", test_home.join("AppData").join("Local"));
    }
}

fn configure_test_daemon_env(
    command: &mut Command,
    daemon_home: &Path,
    control_socket_path: &Path,
    trace_socket_path: &Path,
) {
    command.env("GIT_AI_DAEMON_HOME", daemon_home);
    command.env("GIT_AI_DAEMON_CONTROL_SOCKET", control_socket_path);
    command.env("GIT_AI_DAEMON_TRACE_SOCKET", trace_socket_path);
}

struct DaemonGuard {
    child: Child,
    control_socket_path: PathBuf,
    trace_socket_path: PathBuf,
    repo_working_dir: String,
}

impl DaemonGuard {
    fn start(repo: &TestRepo) -> Self {
        let daemon_home = repo.daemon_home_path();
        let control_socket_path = daemon_control_socket_path(repo);
        let trace_socket_path = daemon_trace_socket_path(repo);
        let mut command = Command::new(get_binary_path());
        command
            .arg("daemon")
            .arg("run")
            .current_dir(repo.path())
            .env("GIT_AI_TEST_DB_PATH", repo.test_db_path())
            .env("GITAI_TEST_DB_PATH", repo.test_db_path())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_test_home_env(&mut command, repo.test_home_path());
        configure_test_daemon_env(
            &mut command,
            &daemon_home,
            &control_socket_path,
            &trace_socket_path,
        );

        let child = command
            .spawn()
            .expect("failed to spawn git-ai daemon subprocess");
        let mut daemon = Self {
            child,
            control_socket_path,
            trace_socket_path,
            repo_working_dir: repo_workdir_string(repo),
        };
        daemon.wait_until_ready();
        daemon
    }

    fn wait_until_ready(&mut self) {
        for _ in 0..200 {
            if let Some(status) = self
                .child
                .try_wait()
                .expect("failed to poll daemon process status")
            {
                panic!("daemon exited before becoming ready: {}", status);
            }
            let status = send_control_request(
                &self.control_socket_path,
                &ControlRequest::StatusFamily {
                    repo_working_dir: self.repo_working_dir.clone(),
                },
            );
            if status.is_ok()
                && local_socket_connects_with_timeout(
                    &self.trace_socket_path,
                    DAEMON_TEST_PROBE_TIMEOUT,
                )
                .is_ok()
            {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!(
            "daemon did not become ready at {}",
            self.control_socket_path.display()
        );
    }

    fn shutdown(&mut self) {
        if self
            .child
            .try_wait()
            .expect("failed polling daemon process")
            .is_some()
        {
            return;
        }

        let _ = send_control_request(&self.control_socket_path, &ControlRequest::Shutdown);

        for _ in 0..200 {
            if self
                .child
                .try_wait()
                .expect("failed polling daemon process")
                .is_some()
            {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn git_trace_env(trace_socket_path: &Path) -> [(&'static str, String); 2] {
    [
        (
            "GIT_TRACE2_EVENT",
            DaemonConfig::trace2_event_target_for_path(trace_socket_path),
        ),
        ("GIT_TRACE2_EVENT_NESTING", "10".to_string()),
    ]
}

fn traced_git_with_env(
    repo: &TestRepo,
    args: &[&str],
    envs: &[(&str, &str)],
    expected_top_level_completions: &mut u64,
) -> Result<String, String> {
    *expected_top_level_completions += 1;
    repo.git_og_with_env(args, envs)
}

fn wait_for_expected_top_level_completions(
    repo: &TestRepo,
    baseline: u64,
    expected_top_level_completions: u64,
) {
    repo.wait_for_daemon_total_completion_count(
        baseline,
        baseline.saturating_add(expected_top_level_completions),
    );
}

fn completion_entries_for_command(
    repo: &TestRepo,
    command: &str,
) -> Vec<DaemonTestCompletionLogEntry> {
    repo.daemon_completion_entries()
        .into_iter()
        .filter(|entry| entry.primary_command.as_deref() == Some(command))
        .collect()
}

fn async_checkpoint_storage_root(repo: &TestRepo) -> PathBuf {
    repo.daemon_home_path()
        .join(".git-ai")
        .join("internal")
        .join("async-checkpoint-blobs")
}

fn async_checkpoint_capture_dir(repo: &TestRepo, capture_id: &str) -> PathBuf {
    async_checkpoint_storage_root(repo).join(capture_id)
}

fn current_head_sha(repo: &TestRepo) -> String {
    repo.git(&["rev-parse", "HEAD"])
        .expect("failed to read HEAD")
        .trim()
        .to_string()
}

fn write_captured_checkpoint_fixture(
    repo: &TestRepo,
    capture_id: &str,
    manifest: &PreparedCheckpointManifest,
    blob_contents: &[(&str, &str)],
) -> PathBuf {
    let capture_dir = async_checkpoint_capture_dir(repo, capture_id);
    fs::create_dir_all(capture_dir.join("blobs")).expect("failed to create capture fixture dir");
    for (blob_name, content) in blob_contents {
        fs::write(capture_dir.join("blobs").join(blob_name), content)
            .expect("failed to write capture blob");
    }
    fs::write(
        capture_dir.join("manifest.json"),
        serde_json::to_vec(manifest).expect("failed to serialize capture manifest"),
    )
    .expect("failed to write capture manifest");
    capture_dir
}

fn latest_checkpoint_blob_content_for_file(repo: &TestRepo, file_path: &str) -> String {
    let working_log = repo.current_working_logs();
    let checkpoints = working_log
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    let entry = checkpoints
        .iter()
        .rev()
        .flat_map(|checkpoint| checkpoint.entries.iter())
        .find(|entry| entry.file == file_path)
        .unwrap_or_else(|| panic!("missing checkpoint entry for {}", file_path));
    working_log
        .get_file_version(&entry.blob_sha)
        .expect("checkpoint blob should be readable")
}

fn ai_agent_run_result(
    repo: &TestRepo,
    edited_filepaths: Vec<String>,
    dirty_files: Option<HashMap<String, String>>,
) -> AgentRunResult {
    AgentRunResult {
        agent_id: AgentId {
            tool: "test-agent".to_string(),
            id: format!(
                "capture-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system time should be valid")
                    .as_nanos()
            ),
            model: "test-model".to_string(),
        },
        agent_metadata: None,
        checkpoint_kind: CheckpointKind::AiAgent,
        transcript: Some(AiTranscript { messages: vec![] }),
        repo_working_dir: Some(repo.path().to_string_lossy().to_string()),
        edited_filepaths: Some(edited_filepaths),
        will_edit_filepaths: None,
        dirty_files,
    }
}

#[derive(Clone)]
struct WorkdirRaceHarness {
    test_home: PathBuf,
    test_db_path: PathBuf,
    daemon_home: PathBuf,
    control_socket_path: PathBuf,
    trace_socket_path: PathBuf,
}

impl WorkdirRaceHarness {
    fn new(repo: &TestRepo, trace_socket_path: PathBuf) -> Self {
        Self {
            test_home: repo.test_home_path().to_path_buf(),
            test_db_path: repo.test_db_path().to_path_buf(),
            daemon_home: repo.daemon_home_path(),
            control_socket_path: repo.daemon_control_socket_path(),
            trace_socket_path,
        }
    }

    fn run_traced_git(&self, workdir: &Path, args: &[&str]) {
        let mut command = Command::new(real_git_executable());
        command.args(args).current_dir(workdir);
        configure_test_home_env(&mut command, &self.test_home);
        let output = command
            .env("GIT_AI_TEST_DB_PATH", &self.test_db_path)
            .env("GITAI_TEST_DB_PATH", &self.test_db_path)
            .env(
                "GIT_TRACE2_EVENT",
                DaemonConfig::trace2_event_target_for_path(&self.trace_socket_path),
            )
            .env("GIT_TRACE2_EVENT_NESTING", "10")
            .output()
            .expect("failed to execute traced git command");
        assert!(
            output.status.success(),
            "traced git command failed in {}: git {} \nstdout:{}\nstderr:{}",
            workdir.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn run_delegated_checkpoint(&self, workdir: &Path, file_rel: &str) {
        let mut command = Command::new(get_binary_path());
        command
            .args(["checkpoint", "mock_ai", file_rel])
            .current_dir(workdir);
        configure_test_home_env(&mut command, &self.test_home);
        configure_test_daemon_env(
            &mut command,
            &self.daemon_home,
            &self.control_socket_path,
            &self.trace_socket_path,
        );
        let output = command
            .env("GIT_AI_TEST_DB_PATH", &self.test_db_path)
            .env("GITAI_TEST_DB_PATH", &self.test_db_path)
            .env("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")
            .output()
            .expect("failed to execute delegated checkpoint");
        assert!(
            output.status.success(),
            "delegated checkpoint failed in {} for {} \nstdout:{}\nstderr:{}",
            workdir.display(),
            file_rel,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_ai_line_checkpoint_and_add(&self, workdir: &Path, file_rel: &str, line: &str) {
        fs::write(workdir.join(file_rel), format!("{line}\n"))
            .expect("failed writing ai line test file");
        self.run_delegated_checkpoint(workdir, file_rel);
        self.run_traced_git(workdir, &["add", file_rel]);
    }

    fn spawn_worktree_ai_stream(
        &self,
        workdir: PathBuf,
        file_prefix: &str,
        line_prefix: &str,
        file_count: usize,
        commit_message: &str,
    ) -> thread::JoinHandle<()> {
        let harness = self.clone();
        let file_prefix = file_prefix.to_string();
        let line_prefix = line_prefix.to_string();
        let commit_message = commit_message.to_string();

        thread::spawn(move || {
            for idx in 0..file_count {
                let file = format!("{file_prefix}-{idx}.txt");
                let line = format!("{line_prefix}-{idx}");
                harness.write_ai_line_checkpoint_and_add(&workdir, file.as_str(), line.as_str());
            }
            harness.run_traced_git(&workdir, &["commit", "-m", commit_message.as_str()]);
        })
    }
}

fn unique_worktree_path(repo: &TestRepo, prefix: &str) -> PathBuf {
    repo.path().parent().unwrap_or(repo.path()).join(format!(
        "{}-{}",
        prefix,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

fn parse_blame_line(line: &str) -> (String, String) {
    if let Some(start_paren) = line.find('(')
        && let Some(end_paren) = line.find(')')
    {
        let author_section = &line[start_paren + 1..end_paren];
        let content = line[end_paren + 1..].trim().to_string();

        let parts: Vec<&str> = author_section.split_whitespace().collect();
        let mut author_parts = Vec::new();
        for part in parts {
            if part.chars().next().unwrap_or('a').is_ascii_digit() {
                break;
            }
            author_parts.push(part);
        }
        return (author_parts.join(" "), content);
    }
    ("unknown".to_string(), line.trim().to_string())
}

fn is_ai_author(author: &str) -> bool {
    let author_lower = author.to_lowercase();
    author_lower.contains("mock_ai")
        || author_lower.contains("claude")
        || author_lower.contains("cursor")
        || author_lower.contains("codex")
}

fn assert_blame_lines_for_workdir(
    repo: &TestRepo,
    workdir: &Path,
    file_rel: &str,
    expected: &[(String, bool)],
) {
    let blame_output = repo
        .git_ai_from_working_dir(workdir, &["blame", file_rel])
        .unwrap_or_else(|e| {
            panic!(
                "git-ai blame failed in {} for {}: {}",
                workdir.display(),
                file_rel,
                e
            )
        });
    let actual: Vec<(String, String)> = blame_output
        .lines()
        .filter(|line: &&str| !line.trim().is_empty())
        .map(parse_blame_line)
        .collect();
    assert_eq!(
        actual.len(),
        expected.len(),
        "line count mismatch for {} in {}\nblame:\n{}",
        file_rel,
        workdir.display(),
        blame_output
    );

    for (idx, ((author, content), (expected_content, expected_ai))) in
        actual.iter().zip(expected.iter()).enumerate()
    {
        assert_eq!(
            content,
            expected_content,
            "line {} content mismatch for {} in {}",
            idx + 1,
            file_rel,
            workdir.display()
        );
        let actual_ai = is_ai_author(author);
        assert_eq!(
            actual_ai,
            *expected_ai,
            "line {} attribution mismatch for {} in {} (author='{}', line='{}')",
            idx + 1,
            file_rel,
            workdir.display(),
            author,
            content
        );
    }
}

fn assert_single_ai_line_for_workdir(repo: &TestRepo, workdir: &Path, file_rel: &str, line: &str) {
    assert_blame_lines_for_workdir(repo, workdir, file_rel, &[(line.to_string(), true)]);
}

fn rewrite_log_path(repo: &TestRepo) -> PathBuf {
    git_common_dir(repo).join("ai").join("rewrite_log")
}

fn rewrite_event_count(repo: &TestRepo, marker: &str) -> usize {
    let path = rewrite_log_path(repo);
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(marker))
        .count()
}

fn wait_for_rewrite_event_count(repo: &TestRepo, marker: &str, expected_count: usize) -> usize {
    let mut observed = 0usize;
    for _ in 0..200 {
        observed = rewrite_event_count(repo, marker);
        if observed >= expected_count {
            return observed;
        }
        thread::sleep(Duration::from_millis(25));
    }
    observed
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[test]
#[serial]
fn daemon_start_spawns_detached_run_process() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    let mut command = Command::new(get_binary_path());
    command
        .arg("daemon")
        .arg("start")
        .current_dir(repo.path())
        .env("GIT_AI_TEST_DB_PATH", repo.test_db_path())
        .env("GITAI_TEST_DB_PATH", repo.test_db_path());
    configure_test_home_env(&mut command, repo.test_home_path());
    configure_test_daemon_env(
        &mut command,
        &repo.daemon_home_path(),
        &daemon_control_socket_path(&repo),
        &daemon_trace_socket_path(&repo),
    );
    let output = command.output().expect("failed to invoke daemon start");
    assert!(
        output.status.success(),
        "daemon start should return success: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut status_ok = false;
    for _ in 0..80 {
        match send_control_request(
            &daemon_control_socket_path(&repo),
            &ControlRequest::StatusFamily {
                repo_working_dir: repo_workdir_string(&repo),
            },
        ) {
            Ok(response) if response.ok => {
                status_ok = true;
                break;
            }
            _ => {
                thread::sleep(Duration::from_millis(25));
            }
        }
    }
    assert!(status_ok, "daemon should be reachable after `daemon start`");

    let _ = send_control_request(
        &daemon_control_socket_path(&repo),
        &ControlRequest::Shutdown,
    );
}

#[test]
#[serial]
fn checkpoint_delegate_autostarts_daemon_when_unavailable() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    fs::write(repo.path().join("delegate-fallback.txt"), "base\n").expect("failed to write base");
    repo.git(&["add", "delegate-fallback.txt"])
        .expect("add should succeed");
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    fs::write(
        repo.path().join("delegate-fallback.txt"),
        "base\nchanged without daemon\n",
    )
    .expect("failed to write updated file");

    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "delegate-fallback.txt"],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("checkpoint should auto-start daemon and succeed");

    let status = send_control_request(
        &daemon_control_socket_path(&repo),
        &ControlRequest::StatusFamily {
            repo_working_dir: repo_workdir_string(&repo),
        },
    )
    .expect("daemon status request should succeed after auto-start");
    assert!(
        status.ok,
        "daemon should be running after delegated checkpoint auto-start"
    );
    let _ = send_control_request(
        &daemon_control_socket_path(&repo),
        &ControlRequest::Shutdown,
    );

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    assert!(
        checkpoints
            .iter()
            .any(|checkpoint| checkpoint.kind == CheckpointKind::AiAgent),
        "delegated checkpoint should write ai_agent checkpoint after daemon auto-start"
    );
}

#[test]
#[serial]
fn checkpoint_delegate_falls_back_when_daemon_startup_is_blocked() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    fs::write(repo.path().join("delegate-fallback-blocked.txt"), "base\n")
        .expect("failed to write base");
    repo.git(&["add", "delegate-fallback-blocked.txt"])
        .expect("add should succeed");
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    fs::write(
        repo.path().join("delegate-fallback-blocked.txt"),
        "base\nchanged while startup blocked\n",
    )
    .expect("failed to write updated file");

    fs::create_dir_all(
        daemon_lock_path(&repo)
            .parent()
            .expect("daemon lock path should have a parent"),
    )
    .expect("failed to create daemon lock parent directory");
    let held_lock = DaemonLock::acquire(&daemon_lock_path(&repo))
        .expect("should acquire daemon lock before checkpoint invocation");

    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "delegate-fallback-blocked.txt"],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("checkpoint should fall back to local mode when daemon startup is blocked");

    drop(held_lock);

    assert!(
        send_control_request(
            &daemon_control_socket_path(&repo),
            &ControlRequest::StatusFamily {
                repo_working_dir: repo_workdir_string(&repo),
            },
        )
        .is_err(),
        "daemon should remain unavailable when startup was blocked"
    );

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    assert!(
        checkpoints
            .iter()
            .any(|checkpoint| checkpoint.kind == CheckpointKind::AiAgent),
        "local fallback should still write ai_agent checkpoint when daemon startup is blocked"
    );
}

#[test]
#[serial]
fn daemon_write_mode_applies_delegated_checkpoint_and_updates_state() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let completion_baseline = repo.daemon_total_completion_count();

    fs::write(repo.path().join("delegate-write.txt"), "base\n").expect("failed to write base");
    repo.git(&["add", "delegate-write.txt"])
        .expect("add should succeed");
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    fs::write(
        repo.path().join("delegate-write.txt"),
        "base\nwritten by delegated checkpoint\n",
    )
    .expect("failed to write updated file");

    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "delegate-write.txt"],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("delegated checkpoint should succeed");

    wait_for_expected_top_level_completions(&repo, completion_baseline, 1);

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    assert!(
        checkpoints
            .iter()
            .any(|checkpoint| checkpoint.kind == CheckpointKind::AiAgent),
        "write-mode daemon should execute checkpoint side effect"
    );
}

#[test]
#[serial]
fn daemon_test_mode_git_ai_checkpoint_runs_via_daemon() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::Dedicated);

    fs::write(repo.path().join("daemon-mode-checkpoint.txt"), "base\n")
        .expect("failed to write base");
    repo.git(&["add", "daemon-mode-checkpoint.txt"])
        .expect("add should succeed");
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    fs::write(
        repo.path().join("daemon-mode-checkpoint.txt"),
        "base\nchanged through daemon mode\n",
    )
    .expect("failed to write updated file");
    let completion_baseline = repo.daemon_total_completion_count();

    let output = repo
        .git_ai(&["checkpoint", "mock_ai", "daemon-mode-checkpoint.txt"])
        .expect("daemon-mode checkpoint should succeed");
    assert!(
        !output.contains("[BENCHMARK] Starting checkpoint run"),
        "daemon-mode checkpoint should not run the local checkpoint implementation: {}",
        output
    );
    assert!(
        output.contains("Checkpoint queued"),
        "explicit-path daemon-mode checkpoint should queue asynchronously: {}",
        output
    );

    repo.wait_for_next_daemon_checkpoint_completion(completion_baseline);

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    assert!(
        checkpoints
            .iter()
            .any(|checkpoint| checkpoint.kind == CheckpointKind::AiAgent),
        "daemon-mode checkpoint should still write the ai_agent checkpoint side effect"
    );
}

#[test]
#[serial]
fn daemon_test_mode_pathless_mock_ai_uses_waited_live_checkpoint_path() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::Dedicated);

    fs::write(repo.path().join("daemon-mode-pathless.txt"), "base\n")
        .expect("failed to write base");
    repo.git(&["add", "daemon-mode-pathless.txt"])
        .expect("add should succeed");
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    fs::write(
        repo.path().join("daemon-mode-pathless.txt"),
        "base\nchanged through waited live daemon path\n",
    )
    .expect("failed to write updated file");
    let completion_baseline = repo.daemon_total_completion_count();

    let output = repo
        .git_ai(&["checkpoint", "mock_ai"])
        .expect("pathless daemon-mode checkpoint should succeed");
    assert!(
        !output.contains("[BENCHMARK] Starting checkpoint run"),
        "pathless daemon-mode checkpoint should still execute via daemon: {}",
        output
    );
    assert!(
        output.contains("Checkpoint completed"),
        "pathless checkpoint should keep the waited live path messaging: {}",
        output
    );
    assert!(
        !output.contains("Checkpoint queued"),
        "pathless checkpoint must not use captured async mode: {}",
        output
    );
    assert_eq!(
        repo.daemon_total_completion_count(),
        completion_baseline.saturating_add(1),
        "waited live checkpoint should complete before the command returns"
    );

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    assert!(
        checkpoints
            .iter()
            .any(|checkpoint| checkpoint.kind == CheckpointKind::AiAgent),
        "pathless daemon-mode checkpoint should still write the ai_agent checkpoint side effect"
    );
}

#[test]
#[serial]
fn daemon_captured_checkpoint_replay_uses_blob_snapshot_after_worktree_changes() {
    let repo = TestRepo::new_with_mode(GitTestMode::Daemon);
    let _daemon = DaemonGuard::start(&repo);

    fs::write(repo.path().join("captured-race.txt"), "base\n").expect("failed to write base");
    repo.git(&["add", "captured-race.txt"])
        .expect("add should succeed");
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    fs::write(
        repo.path().join("captured-race.txt"),
        "snapshot from capture\n",
    )
    .expect("failed to write captured contents");

    let capture_id = "captured-race-fixture";
    let capture_dir = write_captured_checkpoint_fixture(
        &repo,
        capture_id,
        &PreparedCheckpointManifest {
            repo_working_dir: repo.path().to_string_lossy().to_string(),
            base_commit: current_head_sha(&repo),
            captured_at_ms: 1_700_000_000_000,
            kind: CheckpointKind::AiAgent,
            author: "Test User".to_string(),
            reset: false,
            is_pre_commit: false,
            explicit_path_role: PreparedPathRole::Edited,
            explicit_paths: vec!["captured-race.txt".to_string()],
            files: vec![PreparedCheckpointFile {
                path: "captured-race.txt".to_string(),
                source: PreparedCheckpointFileSource::BlobRef {
                    blob_name: "captured-race-blob".to_string(),
                },
            }],
            agent_run_result: Some(ai_agent_run_result(
                &repo,
                vec!["captured-race.txt".to_string()],
                None,
            )),
        },
        &[("captured-race-blob", "snapshot from capture\n")],
    );

    fs::write(
        repo.path().join("captured-race.txt"),
        "live worktree changed later\n",
    )
    .expect("failed to write post-capture contents");

    let response = send_control_request(
        &_daemon.control_socket_path,
        &ControlRequest::CheckpointRun {
            request: Box::new(CheckpointRunRequest::Captured(
                CapturedCheckpointRunRequest {
                    repo_working_dir: repo_workdir_string(&repo),
                    capture_id: capture_id.to_string(),
                },
            )),
            wait: Some(true),
        },
    )
    .expect("captured replay request should succeed");
    assert!(
        response.ok,
        "captured replay should succeed: {:?}",
        response.error
    );

    assert_eq!(
        latest_checkpoint_blob_content_for_file(&repo, "captured-race.txt"),
        "snapshot from capture\n"
    );
    assert!(
        !capture_dir.exists(),
        "captured checkpoint fixture should be deleted after replay"
    );
}

#[test]
#[serial]
fn daemon_captured_checkpoint_replay_supports_mixed_dirty_and_blob_sources() {
    let repo = TestRepo::new_with_mode(GitTestMode::Daemon);
    let _daemon = DaemonGuard::start(&repo);

    fs::write(repo.path().join("dirty-source.txt"), "base dirty\n").expect("failed to write base");
    fs::write(repo.path().join("blob-source.txt"), "base blob\n").expect("failed to write base");
    repo.git(&["add", "dirty-source.txt", "blob-source.txt"])
        .expect("add should succeed");
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    fs::write(
        repo.path().join("dirty-source.txt"),
        "live dirty after capture\n",
    )
    .expect("failed to write live dirty contents");
    fs::write(
        repo.path().join("blob-source.txt"),
        "live blob after capture\n",
    )
    .expect("failed to write live blob contents");

    let capture_id = "captured-mixed-sources";
    let capture_dir = write_captured_checkpoint_fixture(
        &repo,
        capture_id,
        &PreparedCheckpointManifest {
            repo_working_dir: repo.path().to_string_lossy().to_string(),
            base_commit: current_head_sha(&repo),
            captured_at_ms: 1_700_000_000_001,
            kind: CheckpointKind::AiAgent,
            author: "Test User".to_string(),
            reset: false,
            is_pre_commit: false,
            explicit_path_role: PreparedPathRole::Edited,
            explicit_paths: vec![
                "dirty-source.txt".to_string(),
                "blob-source.txt".to_string(),
            ],
            files: vec![
                PreparedCheckpointFile {
                    path: "dirty-source.txt".to_string(),
                    source: PreparedCheckpointFileSource::DirtyFileContent {
                        content: "captured from dirty map\n".to_string(),
                    },
                },
                PreparedCheckpointFile {
                    path: "blob-source.txt".to_string(),
                    source: PreparedCheckpointFileSource::BlobRef {
                        blob_name: "mixed-blob-source".to_string(),
                    },
                },
            ],
            agent_run_result: Some(ai_agent_run_result(
                &repo,
                vec![
                    "dirty-source.txt".to_string(),
                    "blob-source.txt".to_string(),
                ],
                Some(HashMap::from([(
                    "dirty-source.txt".to_string(),
                    "captured from dirty map\n".to_string(),
                )])),
            )),
        },
        &[("mixed-blob-source", "captured from blob snapshot\n")],
    );

    let response = send_control_request(
        &_daemon.control_socket_path,
        &ControlRequest::CheckpointRun {
            request: Box::new(CheckpointRunRequest::Captured(
                CapturedCheckpointRunRequest {
                    repo_working_dir: repo_workdir_string(&repo),
                    capture_id: capture_id.to_string(),
                },
            )),
            wait: Some(true),
        },
    )
    .expect("mixed captured replay request should succeed");
    assert!(
        response.ok,
        "mixed captured replay should succeed: {:?}",
        response.error
    );

    assert_eq!(
        latest_checkpoint_blob_content_for_file(&repo, "dirty-source.txt"),
        "captured from dirty map\n"
    );
    assert_eq!(
        latest_checkpoint_blob_content_for_file(&repo, "blob-source.txt"),
        "captured from blob snapshot\n"
    );
    assert!(
        !capture_dir.exists(),
        "mixed-source capture fixture should be deleted after replay"
    );
}

#[test]
#[serial]
fn daemon_captured_checkpoint_failure_cleans_up_capture_dir() {
    let repo = TestRepo::new_with_mode(GitTestMode::Daemon);
    let _daemon = DaemonGuard::start(&repo);

    fs::write(repo.path().join("broken-capture.txt"), "base\n").expect("failed to write base");
    repo.git(&["add", "broken-capture.txt"])
        .expect("add should succeed");
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    let capture_id = "captured-broken-fixture";
    let capture_dir = write_captured_checkpoint_fixture(
        &repo,
        capture_id,
        &PreparedCheckpointManifest {
            repo_working_dir: repo.path().to_string_lossy().to_string(),
            base_commit: current_head_sha(&repo),
            captured_at_ms: 1_700_000_000_002,
            kind: CheckpointKind::AiAgent,
            author: "Test User".to_string(),
            reset: false,
            is_pre_commit: false,
            explicit_path_role: PreparedPathRole::Edited,
            explicit_paths: vec!["broken-capture.txt".to_string()],
            files: vec![PreparedCheckpointFile {
                path: "broken-capture.txt".to_string(),
                source: PreparedCheckpointFileSource::BlobRef {
                    blob_name: "missing-blob".to_string(),
                },
            }],
            agent_run_result: Some(ai_agent_run_result(
                &repo,
                vec!["broken-capture.txt".to_string()],
                None,
            )),
        },
        &[],
    );

    let response = send_control_request(
        &_daemon.control_socket_path,
        &ControlRequest::CheckpointRun {
            request: Box::new(CheckpointRunRequest::Captured(
                CapturedCheckpointRunRequest {
                    repo_working_dir: repo_workdir_string(&repo),
                    capture_id: capture_id.to_string(),
                },
            )),
            wait: Some(true),
        },
    )
    .expect("broken captured replay request should return a response");
    assert!(
        !response.ok,
        "broken captured replay should fail so cleanup-after-error is exercised"
    );
    assert!(
        !capture_dir.exists(),
        "failed captured replay should still delete the capture fixture"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_commit_after_ai_checkpoint_preserves_ai_replacement_attribution() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let file_path = repo.path().join("daemon-ai-replace.txt");
    let completion_baseline = repo.daemon_total_completion_count();
    let mut expected_top_level_completions = 0u64;

    fs::write(&file_path, "old line\n").expect("failed to write base contents");
    traced_git_with_env(
        &repo,
        &["add", "daemon-ai-replace.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("base add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "base"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("base commit should succeed");

    fs::write(&file_path, "new line from ai\n").expect("failed to write ai contents");
    expected_top_level_completions += 1;
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "daemon-ai-replace.txt"],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("ai checkpoint should succeed");
    traced_git_with_env(
        &repo,
        &["add", "daemon-ai-replace.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "commit ai replacement"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("commit should succeed");

    wait_for_expected_top_level_completions(
        &repo,
        completion_baseline,
        expected_top_level_completions,
    );

    let mut file = repo.filename("daemon-ai-replace.txt");
    file.assert_lines_and_blame(lines!["new line from ai".ai()]);
}

#[test]
#[serial]
fn daemon_trace_ingest_treats_atexit_as_terminal_for_reflog_capture() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let sid = "atexit-commit";
    let completion_baseline = repo.daemon_total_completion_count();

    send_trace_frames(
        &trace_socket,
        &[
            serde_json::json!({
                "event":"start",
                "sid":sid,
                "ts":1,
                "argv":["git","commit","-m","x"],
                "cwd":repo.path().to_string_lossy().to_string(),
            }),
            serde_json::json!({
                "event":"atexit",
                "sid":sid,
                "ts":2,
                "code":1
            }),
        ],
    );

    wait_for_expected_top_level_completions(&repo, completion_baseline, 1);

    let commands = completion_entries_for_command(&repo, "commit");
    assert!(
        commands.iter().any(|command| command.exit_code == Some(1)
            && command.status == "ok"
            && command.seq > 0),
        "atexit terminal frames should still produce a tracked commit command"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_checkpoint_stage_checkpoint_two_commits_preserve_ai_lines() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let file_rel = "daemon-two-ai-lines.txt";
    let file_path = repo.path().join(file_rel);
    let completion_baseline = repo.daemon_total_completion_count();
    let mut expected_top_level_completions = 0u64;

    fs::write(&file_path, "base\n").expect("failed to seed base file");
    traced_git_with_env(
        &repo,
        &["add", file_rel],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("base add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "base"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("base commit should succeed");

    {
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&file_path)
            .expect("failed to open file for first append");
        writeln!(f, "test").expect("failed to append first ai line");
    }
    expected_top_level_completions += 1;
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", file_rel],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("first delegated ai checkpoint should succeed");

    traced_git_with_env(
        &repo,
        &["add", "."],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("staging first ai line should succeed");

    {
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&file_path)
            .expect("failed to open file for second append");
        writeln!(f, "test1").expect("failed to append second ai line");
    }
    expected_top_level_completions += 1;
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", file_rel],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("second delegated ai checkpoint should succeed");

    traced_git_with_env(
        &repo,
        &["commit", "-m", "first ai line"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("first commit should succeed");
    wait_for_expected_top_level_completions(
        &repo,
        completion_baseline,
        expected_top_level_completions,
    );

    traced_git_with_env(
        &repo,
        &["add", "."],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("staging second ai line should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "second ai line"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("second commit should succeed");
    wait_for_expected_top_level_completions(
        &repo,
        completion_baseline,
        expected_top_level_completions,
    );

    let mut file = repo.filename(file_rel);
    file.assert_lines_and_blame(lines!["base", "test".ai(), "test1".ai()]);
}

#[test]
#[serial]
fn daemon_pure_trace_socket_checkpoint_stage_checkpoint_non_adjacent_hunks_survive_split_commits() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let file_rel = "daemon-non-adjacent.md";
    let file_path = repo.path().join(file_rel);
    let completion_baseline = repo.daemon_total_completion_count();
    let mut expected_top_level_completions = 0u64;

    let initial = "\
Top line

**Section Alpha**
alpha body

middle line 1
middle line 2

**Section Omega**
omega body
";
    fs::write(&file_path, initial).expect("failed to write initial content");
    traced_git_with_env(
        &repo,
        &["add", file_rel],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("base add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "base"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("base commit should succeed");

    let first_ai_hunk = "\
Top line

### Section Alpha
alpha body

middle line 1
middle line 2

**Section Omega**
omega body
";
    fs::write(&file_path, first_ai_hunk).expect("failed to write first hunk content");
    expected_top_level_completions += 1;
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", file_rel],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("first delegated checkpoint should succeed");

    traced_git_with_env(
        &repo,
        &["add", "."],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("staging first hunk should succeed");

    let both_hunks = "\
Top line

### Section Alpha
alpha body

middle line 1
middle line 2

### Section Omega
omega body
";
    fs::write(&file_path, both_hunks).expect("failed to write both hunks content");
    expected_top_level_completions += 1;
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", file_rel],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("second delegated checkpoint should succeed");

    traced_git_with_env(
        &repo,
        &["commit", "-m", "commit first staged hunk"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("first split commit should succeed");
    wait_for_expected_top_level_completions(
        &repo,
        completion_baseline,
        expected_top_level_completions,
    );

    traced_git_with_env(
        &repo,
        &["add", "."],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("staging remaining hunk should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "commit second hunk"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("second split commit should succeed");
    wait_for_expected_top_level_completions(
        &repo,
        completion_baseline,
        expected_top_level_completions,
    );

    let mut file = repo.filename(file_rel);
    file.assert_lines_and_blame(lines![
        "Top line",
        "".human(),
        "### Section Alpha".ai(),
        "alpha body",
        "".human(),
        "middle line 1",
        "middle line 2",
        "".human(),
        "### Section Omega".ai(),
        "omega body",
    ]);
}

#[test]
#[serial]
fn daemon_pure_trace_socket_write_mode_applies_amend_rewrite() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let completion_baseline = repo.daemon_total_completion_count();
    let mut expected_top_level_completions = 0u64;

    fs::write(repo.path().join("pure-trace.txt"), "line 1\n").expect("failed to write file");
    traced_git_with_env(
        &repo,
        &["add", "pure-trace.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "initial"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("commit should succeed");

    fs::write(repo.path().join("pure-trace.txt"), "line 1\nline 2\n")
        .expect("failed to update file");
    traced_git_with_env(
        &repo,
        &["add", "pure-trace.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("add before amend should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "--amend", "-m", "initial amended"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("amend should succeed");

    wait_for_expected_top_level_completions(
        &repo,
        completion_baseline,
        expected_top_level_completions,
    );

    let amend_events = wait_for_rewrite_event_count(&repo, "\"commit_amend\"", 1);
    assert_eq!(
        amend_events, 1,
        "pure trace socket mode should emit exactly one commit_amend rewrite event"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_rebase_abort_emits_abort_event() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let default_branch = repo.current_branch();
    let completion_baseline = repo.daemon_total_completion_count();
    let mut expected_top_level_completions = 0u64;

    fs::write(repo.path().join("rebase-conflict.txt"), "base\n").expect("failed to write base");
    traced_git_with_env(
        &repo,
        &["add", "rebase-conflict.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "base"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("base commit should succeed");

    traced_git_with_env(
        &repo,
        &["checkout", "-b", "feature"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("feature branch checkout should succeed");
    fs::write(repo.path().join("rebase-conflict.txt"), "feature\n")
        .expect("failed to write feature branch change");
    traced_git_with_env(
        &repo,
        &["add", "rebase-conflict.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("feature add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "feature change"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("feature commit should succeed");

    traced_git_with_env(
        &repo,
        &["checkout", default_branch.as_str()],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("checkout default branch should succeed");
    fs::write(repo.path().join("rebase-conflict.txt"), "main\n")
        .expect("failed to write default branch change");
    traced_git_with_env(
        &repo,
        &["add", "rebase-conflict.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("default branch add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "main change"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("default branch commit should succeed");

    traced_git_with_env(
        &repo,
        &["checkout", "feature"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("checkout feature should succeed");
    let rebase_conflict = traced_git_with_env(
        &repo,
        &["rebase", default_branch.as_str()],
        &env_refs,
        &mut expected_top_level_completions,
    );
    assert!(
        rebase_conflict.is_err(),
        "rebase should conflict for abort flow coverage"
    );
    wait_for_expected_top_level_completions(
        &repo,
        completion_baseline,
        expected_top_level_completions,
    );
    traced_git_with_env(
        &repo,
        &["rebase", "--abort"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("rebase abort should succeed");

    wait_for_expected_top_level_completions(
        &repo,
        completion_baseline,
        expected_top_level_completions,
    );

    let rewrite_log_path = git_common_dir(&repo).join("ai").join("rewrite_log");
    let rewrite_log =
        fs::read_to_string(&rewrite_log_path).expect("rewrite log should exist after rebase abort");
    assert!(
        rewrite_log
            .lines()
            .any(|line| line.contains("\"rebase_abort\"")),
        "pure trace socket mode should emit rebase_abort rewrite event"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_cherry_pick_abort_emits_abort_event() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let default_branch = repo.current_branch();
    let completion_baseline = repo.daemon_total_completion_count();
    let mut expected_top_level_completions = 0u64;

    fs::write(repo.path().join("cherry-conflict.txt"), "base\n").expect("failed to write base");
    traced_git_with_env(
        &repo,
        &["add", "cherry-conflict.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "base"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("base commit should succeed");

    traced_git_with_env(
        &repo,
        &["checkout", "-b", "topic"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("topic branch checkout should succeed");
    fs::write(repo.path().join("cherry-conflict.txt"), "topic\n")
        .expect("failed to write topic branch change");
    traced_git_with_env(
        &repo,
        &["add", "cherry-conflict.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("topic add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "topic change"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("topic commit should succeed");
    let topic_sha = repo
        .git(&["rev-parse", "topic"])
        .expect("topic rev-parse should succeed")
        .trim()
        .to_string();

    traced_git_with_env(
        &repo,
        &["checkout", default_branch.as_str()],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("checkout default branch should succeed");
    fs::write(repo.path().join("cherry-conflict.txt"), "main\n")
        .expect("failed to write default branch conflicting change");
    traced_git_with_env(
        &repo,
        &["add", "cherry-conflict.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("default branch add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "main change"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("default branch commit should succeed");

    let cherry_pick_conflict = traced_git_with_env(
        &repo,
        &["cherry-pick", topic_sha.as_str()],
        &env_refs,
        &mut expected_top_level_completions,
    );
    assert!(
        cherry_pick_conflict.is_err(),
        "cherry-pick should conflict for abort flow coverage"
    );
    wait_for_expected_top_level_completions(
        &repo,
        completion_baseline,
        expected_top_level_completions,
    );
    traced_git_with_env(
        &repo,
        &["cherry-pick", "--abort"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("cherry-pick abort should succeed");

    wait_for_expected_top_level_completions(
        &repo,
        completion_baseline,
        expected_top_level_completions,
    );

    let rewrite_log_path = git_common_dir(&repo).join("ai").join("rewrite_log");
    let rewrite_log = fs::read_to_string(&rewrite_log_path)
        .expect("rewrite log should exist after cherry-pick abort");
    assert!(
        rewrite_log
            .lines()
            .any(|line| line.contains("\"cherry_pick_abort\"")),
        "pure trace socket mode should emit cherry_pick_abort rewrite event"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_stash_main_ops_emit_stash_events() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let completion_baseline = repo.daemon_total_completion_count();
    let mut expected_top_level_completions = 0u64;

    fs::write(repo.path().join("stash-case.txt"), "base\n").expect("failed to write base");
    traced_git_with_env(
        &repo,
        &["add", "stash-case.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("base add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "base"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("base commit should succeed");

    fs::write(repo.path().join("stash-case.txt"), "base\nchange one\n")
        .expect("failed to write stash content");
    traced_git_with_env(
        &repo,
        &["stash", "push", "-m", "save one"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("stash push should succeed");
    traced_git_with_env(
        &repo,
        &["stash", "list"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("stash list should succeed");
    traced_git_with_env(
        &repo,
        &["stash", "apply", "stash@{0}"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("stash apply should succeed");

    traced_git_with_env(
        &repo,
        &["reset", "--hard", "HEAD"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("reset hard should succeed");
    traced_git_with_env(
        &repo,
        &["stash", "pop", "stash@{0}"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("stash pop should succeed");

    traced_git_with_env(
        &repo,
        &["add", "stash-case.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("add before commit should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "stash pop result"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("commit after stash pop should succeed");

    fs::write(repo.path().join("stash-case.txt"), "base\nchange two\n")
        .expect("failed to write second stash content");
    traced_git_with_env(
        &repo,
        &["stash", "push", "-m", "save two"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("second stash push should succeed");
    traced_git_with_env(
        &repo,
        &["stash", "drop", "stash@{0}"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("stash drop should succeed");

    wait_for_expected_top_level_completions(
        &repo,
        completion_baseline,
        expected_top_level_completions,
    );

    let rewrite_log_path = git_common_dir(&repo).join("ai").join("rewrite_log");
    let rewrite_log =
        fs::read_to_string(&rewrite_log_path).expect("rewrite log should exist after stash ops");
    for expected_operation in [
        "\"operation\":\"Create\"",
        "\"operation\":\"List\"",
        "\"operation\":\"Apply\"",
        "\"operation\":\"Pop\"",
        "\"operation\":\"Drop\"",
    ] {
        assert!(
            rewrite_log.contains(expected_operation),
            "pure trace stash flow should include {} operation",
            expected_operation
        );
    }
}

#[test]
#[serial]
fn daemon_commit_replay_recovers_stash_restore_when_working_log_is_missing() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::Dedicated);
    let mut file = repo.filename("stash-recover.txt");

    file.set_contents(lines!["base top", "base bottom", ""]);
    repo.stage_all_and_commit("base").unwrap();

    file.insert_at(1, lines!["// AI stash line".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai", "stash-recover.txt"])
        .expect("checkpoint before stash should succeed");

    repo.git(&["stash", "push", "-m", "save ai"])
        .expect("stash push should succeed");
    repo.git(&["stash", "apply", "stash@{0}"])
        .expect("stash apply should succeed");
    repo.sync_daemon_force();

    let head = current_head_sha(&repo);
    let git_ai_repo = repo_storage(&repo);
    git_ai_repo
        .storage
        .delete_working_log_for_base_commit(&head)
        .expect("failed to delete restored stash working log");

    repo.git(&["add", "stash-recover.txt"])
        .expect("add after stash restore should succeed");
    repo.git(&["commit", "-m", "stash restore commit"])
        .expect("commit after stash restore should succeed");

    file = repo.filename("stash-recover.txt");
    file.assert_lines_and_blame(lines![
        "base top".human(),
        "// AI stash line".ai(),
        "base bottom".human(),
    ]);
}

#[test]
#[serial]
fn daemon_pure_trace_socket_reset_modes_emit_reset_kinds() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let completion_baseline = repo.daemon_total_completion_count();
    let mut expected_top_level_completions = 0u64;

    fs::write(repo.path().join("reset-case.txt"), "line 1\n").expect("failed to write file");
    traced_git_with_env(
        &repo,
        &["add", "reset-case.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("add should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "c1"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("c1 should succeed");

    fs::write(repo.path().join("reset-case.txt"), "line 1\nline 2\n")
        .expect("failed to write c2 content");
    traced_git_with_env(
        &repo,
        &["add", "reset-case.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("add c2 should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "c2"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("c2 should succeed");

    fs::write(
        repo.path().join("reset-case.txt"),
        "line 1\nline 2\nline 3\n",
    )
    .expect("failed to write c3 content");
    traced_git_with_env(
        &repo,
        &["add", "reset-case.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("add c3 should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "c3"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("c3 should succeed");

    fs::write(
        repo.path().join("reset-case.txt"),
        "line 1\nline 2\nline 3\nline 4\n",
    )
    .expect("failed to write c4 content");
    traced_git_with_env(
        &repo,
        &["add", "reset-case.txt"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("add c4 should succeed");
    traced_git_with_env(
        &repo,
        &["commit", "-m", "c4"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("c4 should succeed");

    traced_git_with_env(
        &repo,
        &["reset", "--soft", "HEAD~1"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("soft reset should succeed");
    traced_git_with_env(
        &repo,
        &["reset", "--mixed", "HEAD~1"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("mixed reset should succeed");
    traced_git_with_env(
        &repo,
        &["reset", "--hard", "HEAD~1"],
        &env_refs,
        &mut expected_top_level_completions,
    )
    .expect("hard reset should succeed");

    wait_for_expected_top_level_completions(
        &repo,
        completion_baseline,
        expected_top_level_completions,
    );
    let rewrite_log_path = git_common_dir(&repo).join("ai").join("rewrite_log");
    let rewrite_log =
        fs::read_to_string(&rewrite_log_path).expect("rewrite log should exist after reset modes");
    for kind in [
        "\"kind\":\"soft\"",
        "\"kind\":\"mixed\"",
        "\"kind\":\"hard\"",
    ] {
        assert!(
            rewrite_log.contains(kind),
            "pure trace reset flow should include {} rewrite event",
            kind,
        );
    }
}

#[test]
#[serial]
fn daemon_commit_replay_recovers_backward_reset_when_working_log_is_missing() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::Dedicated);
    let mut file = repo.filename("reset-recover.txt");

    file.set_contents(lines!["base", ""]);
    let base_commit = repo.stage_all_and_commit("base").unwrap();

    file.insert_at(1, lines!["// AI feature 1".ai()]);
    repo.stage_all_and_commit("ai feature 1").unwrap();

    file.insert_at(2, lines!["// AI feature 2".ai()]);
    let latest_commit = repo.stage_all_and_commit("ai feature 2").unwrap();
    file.insert_at(3, lines!["// AI feature 3".ai()]);

    repo.git(&["reset", "--soft", &base_commit.commit_sha])
        .expect("backward soft reset should succeed");
    repo.sync_daemon_force();

    let head = current_head_sha(&repo);
    let git_ai_repo = repo_storage(&repo);
    assert!(
        git_ai_repo.storage.has_working_log(&head),
        "precondition failed: daemon did not materialize reset working log before simulated loss"
    );
    git_ai_repo
        .storage
        .rename_working_log(&head, &latest_commit.commit_sha)
        .expect("failed to restore pre-reset working log to simulate missing reset side effect");
    fs::write(
        git_common_dir(&repo).join("ORIG_HEAD"),
        format!("{}\n", "0".repeat(40)),
    )
    .expect("failed to clobber ORIG_HEAD");

    repo.stage_all_and_commit("after backward reset")
        .expect("commit after backward reset should succeed");

    file = repo.filename("reset-recover.txt");
    file.assert_lines_and_blame(lines![
        "base".human(),
        "// AI feature 1".ai(),
        "// AI feature 2".ai(),
        "// AI feature 3".ai(),
    ]);
}

#[test]
#[serial]
fn daemon_commit_replay_recovers_same_head_pathspec_reset_when_working_log_is_missing() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::Dedicated);
    let mut keep = repo.filename("pathspec-keep.txt");
    let mut drop = repo.filename("pathspec-drop.txt");

    keep.set_contents(lines!["keep base", ""]);
    drop.set_contents(lines!["drop base", ""]);
    repo.stage_all_and_commit("base").unwrap();

    keep.insert_at(1, lines!["// keep ai".ai()]);
    drop.insert_at(1, lines!["// drop ai".ai()]);
    repo.git(&["add", "-A"])
        .expect("staging pathspec reset fixtures should succeed");

    let head = current_head_sha(&repo);
    let git_ai_repo = repo_storage(&repo);
    let working_log_dir = git_ai_repo.storage.working_log_for_base_commit(&head).dir;
    let backup_dir = repo.path().join(".git-ai-test-pathspec-reset-backup");
    if backup_dir.exists() {
        fs::remove_dir_all(&backup_dir).expect("failed to clear pathspec reset backup");
    }
    copy_dir_recursive(&working_log_dir, &backup_dir);

    repo.git(&["reset", "HEAD", "pathspec-drop.txt"])
        .expect("pathspec reset should succeed");
    repo.sync_daemon_force();

    git_ai_repo
        .storage
        .delete_working_log_for_base_commit(&head)
        .expect("failed to delete post-reset working log");
    copy_dir_recursive(&backup_dir, &working_log_dir);

    repo.git(&["commit", "-m", "commit keep only"])
        .expect("commit after same-head pathspec reset should succeed");

    let new_head = current_head_sha(&repo);
    let new_working_log = git_ai_repo.storage.working_log_for_base_commit(&new_head);
    let initial = new_working_log.read_initial_attributions();
    let note = repo
        .read_authorship_note(&new_head)
        .expect("keep-only commit should have an authorship note");
    assert!(
        !initial.files.contains_key("pathspec-drop.txt"),
        "reset pathspec should remove AI carryover for the dropped file"
    );
    assert!(
        !initial.files.contains_key("pathspec-keep.txt"),
        "kept file should have been consumed by the commit"
    );
    assert!(
        !note.contains("pathspec-drop.txt"),
        "keep-only commit note should not include the pathspec-reset file"
    );
    assert!(
        note.contains("pathspec-keep.txt"),
        "keep-only commit note should preserve the staged file attribution"
    );

    repo.git(&["add", "pathspec-drop.txt"])
        .expect("staging dropped file after recovery should succeed");
    repo.git(&["commit", "-m", "commit drop later"])
        .expect("second commit should succeed");

    keep = repo.filename("pathspec-keep.txt");
    drop = repo.filename("pathspec-drop.txt");
    keep.assert_lines_and_blame(lines!["keep base".human(), "// keep ai".ai()]);
    drop.assert_lines_and_blame(lines!["drop base".human(), "// drop ai".ai()]);
}

#[test]
#[serial]
fn daemon_commit_replay_recovers_squash_prep_when_working_log_is_missing() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::Dedicated);
    let mut file = repo.filename("squash-recover.txt");
    let mut noise = repo.filename("noise.txt");
    let default_branch = repo.current_branch();

    file.set_contents(lines!["line 1", "line 2", "line 3", ""]);
    repo.stage_all_and_commit("base").unwrap();

    noise.set_contents(lines!["noise"]);
    repo.stage_all_and_commit("noise").unwrap();
    repo.git(&["reset", "--hard", "HEAD~1"])
        .expect("older unrelated reset should succeed");
    repo.sync_daemon_force();

    repo.git(&["checkout", "-b", "feature"])
        .expect("feature checkout should succeed");
    repo.sync_daemon_force();
    file = repo.filename("squash-recover.txt");
    file.insert_at(3, lines!["// feature ai".ai()]);
    repo.stage_all_and_commit("feature ai").unwrap();

    repo.git(&["checkout", &default_branch])
        .expect("main checkout should succeed");
    repo.sync_daemon_force();
    let base_head = current_head_sha(&repo);

    repo.git(&["merge", "--squash", "feature"])
        .expect("merge --squash should succeed");
    repo.sync_daemon_force();

    let git_ai_repo = repo_storage(&repo);
    git_ai_repo
        .storage
        .delete_working_log_for_base_commit(&base_head)
        .expect("failed to delete squash-prepared working log");

    repo.git(&["commit", "-m", "squash commit"])
        .expect("commit after missing squash prep should succeed");

    file = repo.filename("squash-recover.txt");
    file.assert_lines_and_blame(lines![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "// feature ai".ai(),
    ]);
}

#[test]
#[serial]
fn daemon_pure_trace_socket_rebase_continue_emits_complete_event() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = vec![
        (env[0].0, env[0].1.as_str()),
        (env[1].0, env[1].1.as_str()),
        ("GIT_EDITOR", "true"),
    ];
    let default_branch = repo.current_branch();

    fs::write(repo.path().join("rebase-continue.txt"), "base\n").expect("failed to write base");
    repo.git_og_with_env(&["add", "rebase-continue.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
        .expect("base commit should succeed");

    repo.git_og_with_env(&["checkout", "-b", "feature"], &env_refs)
        .expect("feature checkout should succeed");
    fs::write(repo.path().join("rebase-continue.txt"), "feature\n")
        .expect("failed to write feature change");
    repo.git_og_with_env(&["add", "rebase-continue.txt"], &env_refs)
        .expect("feature add should succeed");
    repo.git_og_with_env(&["commit", "-m", "feature change"], &env_refs)
        .expect("feature commit should succeed");

    repo.git_og_with_env(&["checkout", default_branch.as_str()], &env_refs)
        .expect("checkout default should succeed");
    fs::write(repo.path().join("rebase-continue.txt"), "main\n")
        .expect("failed to write main change");
    repo.git_og_with_env(&["add", "rebase-continue.txt"], &env_refs)
        .expect("main add should succeed");
    repo.git_og_with_env(&["commit", "-m", "main change"], &env_refs)
        .expect("main commit should succeed");

    repo.git_og_with_env(&["checkout", "feature"], &env_refs)
        .expect("checkout feature should succeed");
    let rebase_conflict = repo.git_og_with_env(&["rebase", default_branch.as_str()], &env_refs);
    assert!(
        rebase_conflict.is_err(),
        "rebase should conflict before continue"
    );
    wait_for_expected_top_level_completions(&repo, 0, 10);

    fs::write(repo.path().join("rebase-continue.txt"), "resolved\n")
        .expect("failed to write resolved content");
    repo.git_og_with_env(&["add", "rebase-continue.txt"], &env_refs)
        .expect("add resolved should succeed");
    repo.git_og_with_env(&["rebase", "--continue"], &env_refs)
        .expect("rebase continue should succeed");

    wait_for_expected_top_level_completions(&repo, 0, 12);

    let rewrite_log_path = git_common_dir(&repo).join("ai").join("rewrite_log");
    let rewrite_log = fs::read_to_string(&rewrite_log_path)
        .expect("rewrite log should exist after rebase continue");
    assert!(
        rewrite_log
            .lines()
            .any(|line| line.contains("\"rebase_complete\"")),
        "pure trace socket mode should emit rebase_complete for continue flow"
    );
}

#[test]
#[serial]
fn daemon_commit_replay_recovers_switch_migration_when_working_log_is_missing() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::Dedicated);
    let default_branch = repo.current_branch();
    let mut file = repo.filename("switch-recover.txt");
    let mut marker = repo.filename("marker.txt");

    file.set_contents(lines!["base", ""]);
    marker.set_contents(lines!["branch marker", ""]);
    let main_head = repo.stage_all_and_commit("base").unwrap().commit_sha;

    repo.git(&["switch", "-c", "feature"])
        .expect("feature switch should succeed");
    marker.insert_at(1, lines!["feature commit"]);
    let feature_head = repo
        .stage_all_and_commit("feature commit")
        .unwrap()
        .commit_sha;

    repo.git(&["switch", default_branch.as_str()])
        .expect("switch back to default branch should succeed");
    file.insert_at(1, lines!["// AI branch carryover".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai", "switch-recover.txt"])
        .expect("branch carryover checkpoint should succeed");

    repo.git(&["switch", "feature"])
        .expect("switch to feature with carried changes should succeed");
    repo.sync_daemon_force();

    let git_ai_repo = repo_storage(&repo);
    git_ai_repo
        .storage
        .rename_working_log(&feature_head, &main_head)
        .expect("failed to restore old working log to simulate missing switch side effect");

    repo.git(&["add", "switch-recover.txt"])
        .expect("add switched file should succeed");
    repo.git(&["commit", "-m", "switch carryover commit"])
        .expect("commit after switch should succeed");

    file = repo.filename("switch-recover.txt");
    file.assert_lines_and_blame(lines!["base".human(), "// AI branch carryover".ai()]);
}

#[test]
#[serial]
fn daemon_pure_trace_socket_cherry_pick_continue_emits_complete_event() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = vec![
        (env[0].0, env[0].1.as_str()),
        (env[1].0, env[1].1.as_str()),
        ("GIT_EDITOR", "true"),
    ];
    let default_branch = repo.current_branch();

    fs::write(repo.path().join("cherry-continue.txt"), "base\n").expect("failed to write base");
    repo.git_og_with_env(&["add", "cherry-continue.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
        .expect("base commit should succeed");

    repo.git_og_with_env(&["checkout", "-b", "topic"], &env_refs)
        .expect("topic checkout should succeed");
    fs::write(repo.path().join("cherry-continue.txt"), "topic\n")
        .expect("failed to write topic change");
    repo.git_og_with_env(&["add", "cherry-continue.txt"], &env_refs)
        .expect("topic add should succeed");
    repo.git_og_with_env(&["commit", "-m", "topic change"], &env_refs)
        .expect("topic commit should succeed");
    let topic_sha = repo
        .git(&["rev-parse", "topic"])
        .expect("topic rev-parse should succeed")
        .trim()
        .to_string();

    repo.git_og_with_env(&["checkout", default_branch.as_str()], &env_refs)
        .expect("checkout default should succeed");
    fs::write(repo.path().join("cherry-continue.txt"), "main\n")
        .expect("failed to write main conflict change");
    repo.git_og_with_env(&["add", "cherry-continue.txt"], &env_refs)
        .expect("main add should succeed");
    repo.git_og_with_env(&["commit", "-m", "main change"], &env_refs)
        .expect("main commit should succeed");

    let cherry_conflict = repo.git_og_with_env(&["cherry-pick", topic_sha.as_str()], &env_refs);
    assert!(
        cherry_conflict.is_err(),
        "cherry-pick should conflict before continue"
    );
    wait_for_expected_top_level_completions(&repo, 0, 9);

    fs::write(repo.path().join("cherry-continue.txt"), "resolved\n")
        .expect("failed to write resolved cherry content");
    repo.git_og_with_env(&["add", "cherry-continue.txt"], &env_refs)
        .expect("add resolved cherry content should succeed");
    repo.git_og_with_env(&["cherry-pick", "--continue"], &env_refs)
        .expect("cherry-pick continue should succeed");

    wait_for_expected_top_level_completions(&repo, 0, 11);

    let rewrite_log_path = git_common_dir(&repo).join("ai").join("rewrite_log");
    let rewrite_log = fs::read_to_string(&rewrite_log_path)
        .expect("rewrite log should exist after cherry-pick continue");
    assert!(
        rewrite_log
            .lines()
            .any(|line| line.contains("\"cherry_pick_complete\"")),
        "pure trace socket mode should emit cherry_pick_complete for continue flow"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_switch_tracks_success_and_conflict_failure() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let default_branch = repo.current_branch();

    fs::write(repo.path().join("switch-case.txt"), "base\n").expect("failed to write base");
    repo.git_og_with_env(&["add", "switch-case.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
        .expect("base commit should succeed");

    repo.git_og_with_env(&["switch", "-c", "feature"], &env_refs)
        .expect("switch -c feature should succeed");
    fs::write(repo.path().join("switch-case.txt"), "feature branch\n")
        .expect("failed to write feature content");
    repo.git_og_with_env(&["add", "switch-case.txt"], &env_refs)
        .expect("feature add should succeed");
    repo.git_og_with_env(&["commit", "-m", "feature"], &env_refs)
        .expect("feature commit should succeed");

    repo.git_og_with_env(&["switch", default_branch.as_str()], &env_refs)
        .expect("switch back to default branch should succeed");
    repo.git_og_with_env(&["switch", "feature"], &env_refs)
        .expect("switch to feature should succeed");
    repo.git_og_with_env(&["switch", default_branch.as_str()], &env_refs)
        .expect("switch back to default branch should succeed");

    fs::write(repo.path().join("switch-case.txt"), "dirty local change\n")
        .expect("failed to write dirty local change");
    let switch_failure = repo.git_og_with_env(&["switch", "feature"], &env_refs);
    assert!(
        switch_failure.is_err(),
        "switch should fail when local changes would be overwritten"
    );

    wait_for_expected_top_level_completions(&repo, 0, 9);

    let switch_entries = completion_entries_for_command(&repo, "switch");
    let saw_switch_success = switch_entries
        .iter()
        .any(|entry| entry.exit_code == Some(0));
    let saw_switch_failure = switch_entries
        .iter()
        .any(|entry| entry.exit_code.unwrap_or(0) != 0);
    assert!(saw_switch_success, "switch success should be tracked");
    assert!(saw_switch_failure, "switch failure should be tracked");
}

#[test]
#[serial]
fn daemon_pure_trace_socket_checkout_tracks_success_failure_and_new_branch() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let default_branch = repo.current_branch();

    fs::write(repo.path().join("checkout-case.txt"), "base\n").expect("failed to write base");
    repo.git_og_with_env(&["add", "checkout-case.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
        .expect("base commit should succeed");

    repo.git_og_with_env(&["checkout", "-b", "feature"], &env_refs)
        .expect("checkout -b feature should succeed");
    fs::write(repo.path().join("checkout-case.txt"), "feature branch\n")
        .expect("failed to write feature content");
    repo.git_og_with_env(&["add", "checkout-case.txt"], &env_refs)
        .expect("feature add should succeed");
    repo.git_og_with_env(&["commit", "-m", "feature"], &env_refs)
        .expect("feature commit should succeed");

    repo.git_og_with_env(&["checkout", default_branch.as_str()], &env_refs)
        .expect("checkout default should succeed");
    repo.git_og_with_env(&["checkout", "feature"], &env_refs)
        .expect("checkout feature should succeed");
    repo.git_og_with_env(&["checkout", "-b", "hotfix"], &env_refs)
        .expect("checkout -b hotfix should succeed");
    repo.git_og_with_env(&["checkout", default_branch.as_str()], &env_refs)
        .expect("checkout back to default should succeed");

    fs::write(
        repo.path().join("checkout-case.txt"),
        "dirty local change\n",
    )
    .expect("failed to write dirty local change");
    let checkout_failure = repo.git_og_with_env(&["checkout", "feature"], &env_refs);
    assert!(
        checkout_failure.is_err(),
        "checkout should fail when local changes would be overwritten"
    );

    wait_for_expected_top_level_completions(&repo, 0, 10);

    let checkout_entries = completion_entries_for_command(&repo, "checkout");
    let saw_checkout_success = checkout_entries
        .iter()
        .any(|entry| entry.exit_code == Some(0));
    let saw_checkout_failure = checkout_entries
        .iter()
        .any(|entry| entry.exit_code.unwrap_or(0) != 0);
    assert!(saw_checkout_success, "checkout success should be tracked");
    assert!(saw_checkout_failure, "checkout failure should be tracked");
}

#[test]
#[serial]
fn daemon_pure_trace_socket_pull_fast_forward_tracks_pull_command() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let default_branch = repo.current_branch();

    let run_git = |args: &[&str]| -> String {
        let output = Command::new(real_git_executable())
            .args(args)
            .output()
            .expect("git command should execute");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };

    fs::write(repo.path().join("pull-case.txt"), "base\n").expect("failed to write base");
    repo.git_og_with_env(&["add", "pull-case.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
        .expect("base commit should succeed");

    let root = repo
        .path()
        .parent()
        .expect("test repo path should have parent")
        .to_path_buf();
    let bare_remote = root.join("origin.git");
    let remote_clone = root.join("origin-work");
    let bare_remote_str = bare_remote.to_string_lossy().to_string();
    let remote_clone_str = remote_clone.to_string_lossy().to_string();
    let _ = fs::remove_dir_all(&bare_remote);
    let _ = fs::remove_dir_all(&remote_clone);

    run_git(&["init", "--bare", bare_remote_str.as_str()]);
    repo.git_og_with_env(
        &["remote", "add", "origin", bare_remote_str.as_str()],
        &env_refs,
    )
    .expect("adding origin remote should succeed");
    repo.git_og_with_env(
        &["push", "-u", "origin", default_branch.as_str()],
        &env_refs,
    )
    .expect("pushing base branch should succeed");

    run_git(&[
        "clone",
        "--branch",
        default_branch.as_str(),
        bare_remote_str.as_str(),
        remote_clone_str.as_str(),
    ]);
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "config",
        "user.name",
        "Test User",
    ]);
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "config",
        "user.email",
        "test@example.com",
    ]);
    fs::write(remote_clone.join("pull-case.txt"), "base\nremote update\n")
        .expect("failed to write remote update");
    run_git(&["-C", remote_clone_str.as_str(), "add", "pull-case.txt"]);
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "commit",
        "-m",
        "remote update",
    ]);
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "push",
        "origin",
        format!("HEAD:{}", default_branch).as_str(),
    ]);

    repo.git_og_with_env(
        &["pull", "--ff-only", "origin", default_branch.as_str()],
        &env_refs,
    )
    .expect("fast-forward pull should succeed");

    wait_for_expected_top_level_completions(&repo, 0, 5);

    let pull_entries = completion_entries_for_command(&repo, "pull");
    let saw_pull_success = pull_entries.iter().any(|entry| entry.exit_code == Some(0));
    assert!(saw_pull_success, "pull success should be tracked");
    assert!(
        fs::read_to_string(repo.path().join("pull-case.txt"))
            .expect("pulled file should be readable")
            .contains("remote update"),
        "pull fast-forward should update the worktree contents"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_pull_rebase_tracks_pull_and_rebase_completion() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let default_branch = repo.current_branch();

    let run_git = |args: &[&str]| -> String {
        let output = Command::new(real_git_executable())
            .args(args)
            .output()
            .expect("git command should execute");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };

    fs::write(repo.path().join("pull-rebase-base.txt"), "base\n").expect("failed to write base");
    repo.git_og_with_env(&["add", "pull-rebase-base.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
        .expect("base commit should succeed");

    let root = repo
        .path()
        .parent()
        .expect("test repo path should have parent")
        .to_path_buf();
    let bare_remote = root.join("origin-rebase.git");
    let remote_clone = root.join("origin-rebase-work");
    let bare_remote_str = bare_remote.to_string_lossy().to_string();
    let remote_clone_str = remote_clone.to_string_lossy().to_string();
    let _ = fs::remove_dir_all(&bare_remote);
    let _ = fs::remove_dir_all(&remote_clone);

    run_git(&["init", "--bare", bare_remote_str.as_str()]);
    repo.git_og_with_env(
        &["remote", "add", "origin", bare_remote_str.as_str()],
        &env_refs,
    )
    .expect("adding origin remote should succeed");
    repo.git_og_with_env(
        &["push", "-u", "origin", default_branch.as_str()],
        &env_refs,
    )
    .expect("pushing base branch should succeed");

    run_git(&[
        "clone",
        "--branch",
        default_branch.as_str(),
        bare_remote_str.as_str(),
        remote_clone_str.as_str(),
    ]);
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "config",
        "user.name",
        "Test User",
    ]);
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "config",
        "user.email",
        "test@example.com",
    ]);
    fs::write(remote_clone.join("remote-only.txt"), "remote\n")
        .expect("failed to write remote file");
    run_git(&["-C", remote_clone_str.as_str(), "add", "remote-only.txt"]);
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "commit",
        "-m",
        "remote commit",
    ]);
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "push",
        "origin",
        format!("HEAD:{}", default_branch).as_str(),
    ]);

    fs::write(repo.path().join("local-only.txt"), "local\n").expect("failed to write local file");
    repo.git_og_with_env(&["add", "local-only.txt"], &env_refs)
        .expect("local add should succeed");
    repo.git_og_with_env(&["commit", "-m", "local commit"], &env_refs)
        .expect("local commit should succeed");

    repo.git_og_with_env(
        &["pull", "--rebase", "origin", default_branch.as_str()],
        &env_refs,
    )
    .expect("pull --rebase should succeed");

    wait_for_expected_top_level_completions(&repo, 0, 7);

    let pull_entries = completion_entries_for_command(&repo, "pull");
    let saw_pull_rebase_success = pull_entries.iter().any(|entry| entry.exit_code == Some(0));
    assert!(
        saw_pull_rebase_success,
        "pull --rebase success should be tracked"
    );

    let rebase_complete_events = wait_for_rewrite_event_count(&repo, "\"rebase_complete\"", 1);
    assert!(
        rebase_complete_events >= 1,
        "pull --rebase should result in a rebase_complete rewrite signal"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_pull_autostash_preserves_local_changes_and_tracks_command() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let default_branch = repo.current_branch();

    let run_git = |args: &[&str]| -> String {
        let output = Command::new(real_git_executable())
            .args(args)
            .output()
            .expect("git command should execute");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };

    fs::write(repo.path().join("autostash-local.txt"), "base\n").expect("failed to write base");
    repo.git_og_with_env(&["add", "autostash-local.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
        .expect("base commit should succeed");

    let root = repo
        .path()
        .parent()
        .expect("test repo path should have parent")
        .to_path_buf();
    let bare_remote = root.join("origin-autostash.git");
    let remote_clone = root.join("origin-autostash-work");
    let bare_remote_str = bare_remote.to_string_lossy().to_string();
    let remote_clone_str = remote_clone.to_string_lossy().to_string();
    let _ = fs::remove_dir_all(&bare_remote);
    let _ = fs::remove_dir_all(&remote_clone);

    run_git(&["init", "--bare", bare_remote_str.as_str()]);
    repo.git_og_with_env(
        &["remote", "add", "origin", bare_remote_str.as_str()],
        &env_refs,
    )
    .expect("adding origin remote should succeed");
    repo.git_og_with_env(
        &["push", "-u", "origin", default_branch.as_str()],
        &env_refs,
    )
    .expect("pushing base branch should succeed");

    run_git(&[
        "clone",
        "--branch",
        default_branch.as_str(),
        bare_remote_str.as_str(),
        remote_clone_str.as_str(),
    ]);
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "config",
        "user.name",
        "Test User",
    ]);
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "config",
        "user.email",
        "test@example.com",
    ]);
    fs::write(remote_clone.join("autostash-remote.txt"), "remote\n")
        .expect("failed to write remote update file");
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "add",
        "autostash-remote.txt",
    ]);
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "commit",
        "-m",
        "remote update",
    ]);
    run_git(&[
        "-C",
        remote_clone_str.as_str(),
        "push",
        "origin",
        format!("HEAD:{}", default_branch).as_str(),
    ]);

    fs::write(
        repo.path().join("autostash-local.txt"),
        "base\nlocal dirty change\n",
    )
    .expect("failed to write local dirty change");

    repo.git_og_with_env(
        &[
            "pull",
            "--rebase",
            "--autostash",
            "origin",
            default_branch.as_str(),
        ],
        &env_refs,
    )
    .expect("pull --rebase --autostash should succeed");

    wait_for_expected_top_level_completions(&repo, 0, 5);

    let local_contents = fs::read_to_string(repo.path().join("autostash-local.txt"))
        .expect("local file should remain readable");
    assert!(
        local_contents.contains("local dirty change"),
        "autostash pull should preserve local dirty change content"
    );

    let pull_entries = completion_entries_for_command(&repo, "pull");
    let saw_pull_autostash_success = pull_entries.iter().any(|entry| entry.exit_code == Some(0));
    assert!(
        saw_pull_autostash_success,
        "pull --rebase --autostash success should be tracked"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_high_throughput_ai_commit_burst_preserves_exact_blame() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];

    let file_count = 16usize;
    for idx in 0..file_count {
        let file_rel = format!("daemon-race-file-{idx}.txt");
        let file_path = repo.path().join(file_rel.as_str());
        fs::write(&file_path, format!("ai-line-{idx}\n"))
            .expect("failed to write ai burst test file");

        repo.git_ai_with_env(
            &["checkpoint", "mock_ai", file_rel.as_str()],
            &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
        )
        .expect("delegated ai checkpoint should succeed");

        repo.git_og_with_env(&["add", file_rel.as_str()], &env_refs)
            .expect("staging ai burst file should succeed");
    }

    repo.git_og_with_env(&["commit", "-m", "ai burst commit"], &env_refs)
        .expect("ai burst commit should succeed");

    wait_for_expected_top_level_completions(&repo, 0, (file_count as u64 * 2) + 1);
    let commit_events = wait_for_rewrite_event_count(&repo, "\"commit_sha\"", 1);
    assert_eq!(
        commit_events, 1,
        "expected exactly one commit rewrite event for burst commit"
    );

    for idx in 0..file_count {
        let mut file = repo.filename(format!("daemon-race-file-{idx}.txt").as_str());
        file.assert_lines_and_blame(lines![format!("ai-line-{idx}").ai()]);
    }
}

#[test]
#[serial]
fn daemon_pure_trace_socket_concurrent_worktree_burst_preserves_exact_line_attribution() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];

    let harness = WorkdirRaceHarness::new(&repo, trace_socket.clone());
    let worker_a_dir = repo.path().to_path_buf();
    let worker_b_dir = unique_worktree_path(&repo, "daemon-race-worker-b");
    let worker_b_dir_str = worker_b_dir.to_string_lossy().to_string();

    repo.git_og_with_env(&["checkout", "-b", "daemon-race-worker-a"], &env_refs)
        .expect("checkout worker-a branch should succeed");
    repo.git_og_with_env(
        &[
            "worktree",
            "add",
            "-b",
            "daemon-race-worker-b",
            worker_b_dir_str.as_str(),
        ],
        &env_refs,
    )
    .expect("worktree add worker-b should succeed");
    wait_for_expected_top_level_completions(&repo, 0, 2);

    let file_count = 10usize;
    let completion_baseline = repo.daemon_total_completion_count();
    for idx in 0..file_count {
        let file_a = format!("daemon-race-a-{idx}.txt");
        harness.write_ai_line_checkpoint_and_add(
            &worker_a_dir,
            file_a.as_str(),
            format!("a-ai-line-{idx}").as_str(),
        );

        let file_b = format!("daemon-race-b-{idx}.txt");
        harness.write_ai_line_checkpoint_and_add(
            &worker_b_dir,
            file_b.as_str(),
            format!("b-ai-line-{idx}").as_str(),
        );
    }

    harness.run_traced_git(&worker_a_dir, &["commit", "-m", "worker-a burst commit"]);
    harness.run_traced_git(&worker_b_dir, &["commit", "-m", "worker-b burst commit"]);

    let expected_completion_delta = (file_count as u64 * 4) + 2;
    wait_for_expected_top_level_completions(&repo, completion_baseline, expected_completion_delta);

    for idx in 0..file_count {
        let file_a = format!("daemon-race-a-{idx}.txt");
        let file_b = format!("daemon-race-b-{idx}.txt");
        assert_single_ai_line_for_workdir(
            &repo,
            &worker_a_dir,
            file_a.as_str(),
            format!("a-ai-line-{idx}").as_str(),
        );
        assert_single_ai_line_for_workdir(
            &repo,
            &worker_b_dir,
            file_b.as_str(),
            format!("b-ai-line-{idx}").as_str(),
        );
    }

    let _ = repo.git_og_with_env(
        &["worktree", "remove", "--force", worker_b_dir_str.as_str()],
        &env_refs,
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_concurrent_checkpoint_requests_preserve_exact_line_attribution() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];

    let harness = WorkdirRaceHarness::new(&repo, trace_socket.clone());
    let workdir = repo.path().to_path_buf();

    let file_count = 12usize;
    let completion_baseline = repo.daemon_total_completion_count();
    let mut expected = Vec::new();
    for idx in 0..file_count {
        let file_rel = format!("daemon-race-concurrent-checkpoint-{idx}.txt");
        let line = format!("ai-line-{idx}");
        fs::write(workdir.join(file_rel.as_str()), format!("{line}\n"))
            .expect("failed to write concurrent checkpoint test file");
        expected.push((file_rel, line));
    }

    let mut checkpoint_threads = Vec::new();
    for (file_rel, _) in &expected {
        let thread_workdir = workdir.clone();
        let harness = harness.clone();
        let file_rel = file_rel.clone();
        checkpoint_threads.push(thread::spawn(move || {
            harness.run_delegated_checkpoint(&thread_workdir, file_rel.as_str());
        }));
    }
    for handle in checkpoint_threads {
        handle
            .join()
            .expect("concurrent delegated checkpoint thread should not panic");
    }

    repo.git_og_with_env(&["add", "."], &env_refs)
        .expect("staging concurrent checkpoint files should succeed");
    repo.git_og_with_env(
        &["commit", "-m", "concurrent delegated checkpoint burst"],
        &env_refs,
    )
    .expect("commit for concurrent checkpoint files should succeed");

    let expected_completion_delta = file_count as u64 + 2;
    wait_for_expected_top_level_completions(&repo, completion_baseline, expected_completion_delta);

    for (file_rel, line) in expected {
        let mut file = repo.filename(file_rel.as_str());
        file.assert_lines_and_blame(lines![line.ai()]);
    }
}

#[test]
#[serial]
fn daemon_pure_trace_socket_parallel_worktree_streams_preserve_exact_line_attribution() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let _daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];

    let harness = WorkdirRaceHarness::new(&repo, trace_socket.clone());
    let worker_a_dir = repo.path().to_path_buf();
    let worker_b_dir = unique_worktree_path(&repo, "daemon-race-worker-b-parallel");
    let worker_b_dir_str = worker_b_dir.to_string_lossy().to_string();

    repo.git_og_with_env(
        &["checkout", "-b", "daemon-race-parallel-worker-a"],
        &env_refs,
    )
    .expect("checkout parallel worker-a branch should succeed");
    repo.git_og_with_env(
        &[
            "worktree",
            "add",
            "-b",
            "daemon-race-parallel-worker-b",
            worker_b_dir_str.as_str(),
        ],
        &env_refs,
    )
    .expect("worktree add parallel worker-b should succeed");
    wait_for_expected_top_level_completions(&repo, 0, 2);

    let file_count = 8usize;
    let completion_baseline = repo.daemon_total_completion_count();

    let worker_a = harness.spawn_worktree_ai_stream(
        worker_a_dir.clone(),
        "daemon-race-parallel-a",
        "a-parallel-ai-line",
        file_count,
        "parallel worker-a commit",
    );

    let worker_b = harness.spawn_worktree_ai_stream(
        worker_b_dir.clone(),
        "daemon-race-parallel-b",
        "b-parallel-ai-line",
        file_count,
        "parallel worker-b commit",
    );

    worker_a
        .join()
        .expect("parallel worker-a thread should not panic");
    worker_b
        .join()
        .expect("parallel worker-b thread should not panic");

    let expected_completion_delta = ((file_count as u64) * 2 + 1) * 2;
    wait_for_expected_top_level_completions(&repo, completion_baseline, expected_completion_delta);

    for idx in 0..file_count {
        let file_a = format!("daemon-race-parallel-a-{idx}.txt");
        let file_b = format!("daemon-race-parallel-b-{idx}.txt");
        assert_single_ai_line_for_workdir(
            &repo,
            &worker_a_dir,
            file_a.as_str(),
            format!("a-parallel-ai-line-{idx}").as_str(),
        );
        assert_single_ai_line_for_workdir(
            &repo,
            &worker_b_dir,
            file_b.as_str(),
            format!("b-parallel-ai-line-{idx}").as_str(),
        );
    }

    let _ = repo.git_og_with_env(
        &["worktree", "remove", "--force", worker_b_dir_str.as_str()],
        &env_refs,
    );
}
