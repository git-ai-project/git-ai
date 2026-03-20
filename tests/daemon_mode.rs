#[macro_use]
#[path = "integration/repos/mod.rs"]
mod repos;

use git_ai::authorship::working_log::CheckpointKind;
use git_ai::daemon::{
    ControlRequest, ControlResponse, DaemonConfig, DaemonLock, send_control_request,
};
use interprocess::local_socket::LocalSocketStream;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::{GitTestMode, TestRepo, get_binary_path, real_git_executable};
use serde_json::Value;
use serial_test::serial;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

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

fn daemon_control_socket_path(repo: &TestRepo) -> PathBuf {
    DaemonConfig::from_home(repo.test_home_path()).control_socket_path
}

fn daemon_trace_socket_path(repo: &TestRepo) -> PathBuf {
    DaemonConfig::from_home(repo.test_home_path()).trace_socket_path
}

fn daemon_lock_path(repo: &TestRepo) -> PathBuf {
    DaemonConfig::from_home(repo.test_home_path()).lock_path
}

fn send_trace_frames(trace_socket_path: &Path, payloads: &[Value]) {
    let mut stream = LocalSocketStream::connect(trace_socket_path.to_string_lossy().as_ref())
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

fn open_trace_stream(trace_socket_path: &Path) -> LocalSocketStream {
    LocalSocketStream::connect(trace_socket_path.to_string_lossy().as_ref())
        .expect("failed to connect to trace socket")
}

fn write_trace_frames(stream: &mut LocalSocketStream, payloads: &[Value]) {
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

struct DaemonGuard {
    child: Child,
    control_socket_path: PathBuf,
    trace_socket_path: PathBuf,
    repo_working_dir: String,
}

impl DaemonGuard {
    fn start(repo: &TestRepo) -> Self {
        let mut command = Command::new(get_binary_path());
        command
            .arg("daemon")
            .arg("run")
            .current_dir(repo.path())
            .env("HOME", repo.test_home_path())
            .env(
                "GIT_CONFIG_GLOBAL",
                repo.test_home_path().join(".gitconfig"),
            )
            .env("GIT_AI_TEST_DB_PATH", repo.test_db_path())
            .env("GITAI_TEST_DB_PATH", repo.test_db_path())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = command
            .spawn()
            .expect("failed to spawn git-ai daemon subprocess");
        let mut daemon = Self {
            child,
            control_socket_path: daemon_control_socket_path(repo),
            trace_socket_path: daemon_trace_socket_path(repo),
            repo_working_dir: repo_workdir_string(repo),
        };
        daemon.wait_until_ready();
        daemon
    }

    fn request(&self, request: ControlRequest) -> ControlResponse {
        send_control_request(&self.control_socket_path, &request)
            .unwrap_or_else(|e| panic!("control request failed: {}", e))
    }

    fn latest_seq_and_wait_idle(&self) -> u64 {
        let settled = self.request(ControlRequest::WaitFamilyIdle {
            repo_working_dir: self.repo_working_dir.clone(),
            timeout_ms: Some(8_000),
        });
        assert!(settled.ok, "wait.family_idle should succeed");
        let status = self.request(ControlRequest::StatusFamily {
            repo_working_dir: self.repo_working_dir.clone(),
        });
        assert!(status.ok, "status request should succeed");
        status
            .data
            .as_ref()
            .and_then(|v| v.get("latest_seq"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    fn family_state_snapshot(&self) -> Value {
        let response = self.request(ControlRequest::SnapshotFamily {
            repo_working_dir: self.repo_working_dir.clone(),
        });
        assert!(response.ok, "snapshot request should succeed");
        response
            .data
            .as_ref()
            .and_then(|v| v.get("state"))
            .cloned()
            .expect("snapshot response should include state payload")
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
            if self.control_socket_path.exists() && self.trace_socket_path.exists() {
                let status = send_control_request(
                    &self.control_socket_path,
                    &ControlRequest::StatusFamily {
                        repo_working_dir: self.repo_working_dir.clone(),
                    },
                );
                if status.is_ok() {
                    return;
                }
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
            format!("af_unix:stream:{}", trace_socket_path.to_string_lossy()),
        ),
        ("GIT_TRACE2_EVENT_NESTING", "10".to_string()),
    ]
}

#[derive(Clone)]
struct WorkdirRaceHarness {
    test_home: PathBuf,
    test_db_path: PathBuf,
    trace_socket_path: PathBuf,
}

impl WorkdirRaceHarness {
    fn new(repo: &TestRepo, trace_socket_path: PathBuf) -> Self {
        Self {
            test_home: repo.test_home_path().to_path_buf(),
            test_db_path: repo.test_db_path().to_path_buf(),
            trace_socket_path,
        }
    }

    fn run_traced_git(&self, workdir: &Path, args: &[&str]) {
        let output = Command::new(real_git_executable())
            .args(args)
            .current_dir(workdir)
            .env("HOME", &self.test_home)
            .env("GIT_CONFIG_GLOBAL", self.test_home.join(".gitconfig"))
            .env("GIT_AI_TEST_DB_PATH", &self.test_db_path)
            .env("GITAI_TEST_DB_PATH", &self.test_db_path)
            .env(
                "GIT_TRACE2_EVENT",
                format!(
                    "af_unix:stream:{}",
                    self.trace_socket_path.to_string_lossy()
                ),
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
        let output = Command::new(get_binary_path())
            .args(["checkpoint", "mock_ai", file_rel])
            .current_dir(workdir)
            .env("HOME", &self.test_home)
            .env("GIT_CONFIG_GLOBAL", self.test_home.join(".gitconfig"))
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

    let output = Command::new(get_binary_path())
        .arg("daemon")
        .arg("start")
        .current_dir(repo.path())
        .env("HOME", repo.test_home_path())
        .env(
            "GIT_CONFIG_GLOBAL",
            repo.test_home_path().join(".gitconfig"),
        )
        .env("GIT_AI_TEST_DB_PATH", repo.test_db_path())
        .env("GITAI_TEST_DB_PATH", repo.test_db_path())
        .output()
        .expect("failed to invoke daemon start");
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
    let daemon = DaemonGuard::start(&repo);

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

    daemon.latest_seq_and_wait_idle();

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

    let family_state = daemon.family_state_snapshot();
    let checkpoints_map = family_state
        .get("checkpoints")
        .and_then(Value::as_object)
        .expect("family state should contain checkpoints map");
    assert!(
        !checkpoints_map.is_empty(),
        "daemon family state should record delegated checkpoint summary"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_commit_after_ai_checkpoint_preserves_ai_replacement_attribution() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let file_path = repo.path().join("daemon-ai-replace.txt");

    fs::write(&file_path, "old line\n").expect("failed to write base contents");
    repo.git_og_with_env(&["add", "daemon-ai-replace.txt"], &env_refs)
        .expect("base add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
        .expect("base commit should succeed");

    fs::write(&file_path, "new line from ai\n").expect("failed to write ai contents");
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "daemon-ai-replace.txt"],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("ai checkpoint should succeed");
    repo.git_og_with_env(&["add", "daemon-ai-replace.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "commit ai replacement"], &env_refs)
        .expect("commit should succeed");

    daemon.latest_seq_and_wait_idle();

    let mut file = repo.filename("daemon-ai-replace.txt");
    file.assert_lines_and_blame(lines!["new line from ai".ai()]);
}

#[test]
#[serial]
fn daemon_trace_ingest_treats_atexit_as_terminal_for_reflog_capture() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let sid = "atexit-commit";

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

    daemon.latest_seq_and_wait_idle();

    let family_state = daemon.family_state_snapshot();
    let commands = family_state
        .get("commands")
        .and_then(Value::as_array)
        .expect("family state should contain command history");
    assert!(
        commands.iter().any(|command| {
            command.get("sid").and_then(Value::as_str) == Some(sid)
                && command.get("name").and_then(Value::as_str) == Some("commit")
                && command.get("exit_code").and_then(Value::as_i64) == Some(1)
        }),
        "atexit terminal frames should still produce a tracked commit command"
    );
}

#[test]
#[serial]
fn daemon_status_family_ignores_open_roots_from_other_families() {
    let repo_a = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let repo_b = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo_a);
    let mut trace_stream = open_trace_stream(&daemon.trace_socket_path);

    write_trace_frames(
        &mut trace_stream,
        &[serde_json::json!({
            "event":"start",
            "sid":"family-b-open-root",
            "ts":1,
            "argv":["git","status"],
            "cwd":repo_b.path().to_string_lossy().to_string(),
        })],
    );

    let status_a = daemon.request(ControlRequest::StatusFamily {
        repo_working_dir: repo_workdir_string(&repo_a),
    });
    assert!(status_a.ok, "family A status should succeed");
    let status_a_data = status_a
        .data
        .as_ref()
        .expect("family A status should include data");
    assert_eq!(
        status_a_data.get("backlog").and_then(Value::as_u64),
        Some(0),
        "family A backlog should ignore family B roots: {}",
        status_a_data
    );
    assert_eq!(
        status_a_data.get("pending_roots").and_then(Value::as_u64),
        Some(0),
        "family A pending_roots should ignore family B roots: {}",
        status_a_data
    );

    let status_b = daemon.request(ControlRequest::StatusFamily {
        repo_working_dir: repo_workdir_string(&repo_b),
    });
    assert!(status_b.ok, "family B status should succeed");
    let status_b_data = status_b
        .data
        .as_ref()
        .expect("family B status should include data");
    assert_eq!(
        status_b_data.get("pending_roots").and_then(Value::as_u64),
        Some(1),
        "family B should report its own open root: {}",
        status_b_data
    );
}

#[test]
#[serial]
fn daemon_checkpoint_wait_is_not_blocked_by_other_family_open_root() {
    let repo_a = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let repo_b = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo_a);
    let mut trace_stream = open_trace_stream(&daemon.trace_socket_path);

    write_trace_frames(
        &mut trace_stream,
        &[serde_json::json!({
            "event":"start",
            "sid":"family-b-open-root-for-checkpoint",
            "ts":1,
            "argv":["git","status"],
            "cwd":repo_b.path().to_string_lossy().to_string(),
        })],
    );

    let response = daemon.request(ControlRequest::CheckpointRun {
        request: Box::new(git_ai::daemon::CheckpointRunRequest {
            repo_working_dir: repo_workdir_string(&repo_a),
            author: Some("mock_ai".to_string()),
            quiet: Some(true),
            ..Default::default()
        }),
        wait: Some(true),
    });
    assert!(
        response.ok,
        "family A checkpoint should not wait on family B open root: {:?}",
        response
    );
    assert!(
        response.applied_seq.is_some(),
        "checkpoint wait should return an applied sequence: {:?}",
        response
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_checkpoint_stage_checkpoint_two_commits_preserve_ai_lines() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let file_rel = "daemon-two-ai-lines.txt";
    let file_path = repo.path().join(file_rel);

    fs::write(&file_path, "base\n").expect("failed to seed base file");
    repo.git_og_with_env(&["add", file_rel], &env_refs)
        .expect("base add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
        .expect("base commit should succeed");

    {
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&file_path)
            .expect("failed to open file for first append");
        writeln!(f, "test").expect("failed to append first ai line");
    }
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", file_rel],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("first delegated ai checkpoint should succeed");

    repo.git_og_with_env(&["add", "."], &env_refs)
        .expect("staging first ai line should succeed");

    {
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&file_path)
            .expect("failed to open file for second append");
        writeln!(f, "test1").expect("failed to append second ai line");
    }
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", file_rel],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("second delegated ai checkpoint should succeed");

    repo.git_og_with_env(&["commit", "-m", "first ai line"], &env_refs)
        .expect("first commit should succeed");
    daemon.latest_seq_and_wait_idle();

    repo.git_og_with_env(&["add", "."], &env_refs)
        .expect("staging second ai line should succeed");
    repo.git_og_with_env(&["commit", "-m", "second ai line"], &env_refs)
        .expect("second commit should succeed");
    daemon.latest_seq_and_wait_idle();

    let mut file = repo.filename(file_rel);
    file.assert_lines_and_blame(lines!["base", "test".ai(), "test1".ai()]);
}

#[test]
#[serial]
fn daemon_pure_trace_socket_checkpoint_stage_checkpoint_non_adjacent_hunks_survive_split_commits() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let file_rel = "daemon-non-adjacent.md";
    let file_path = repo.path().join(file_rel);

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
    repo.git_og_with_env(&["add", file_rel], &env_refs)
        .expect("base add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
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
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", file_rel],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("first delegated checkpoint should succeed");

    repo.git_og_with_env(&["add", "."], &env_refs)
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
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", file_rel],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("second delegated checkpoint should succeed");

    repo.git_og_with_env(&["commit", "-m", "commit first staged hunk"], &env_refs)
        .expect("first split commit should succeed");
    daemon.latest_seq_and_wait_idle();

    repo.git_og_with_env(&["add", "."], &env_refs)
        .expect("staging remaining hunk should succeed");
    repo.git_og_with_env(&["commit", "-m", "commit second hunk"], &env_refs)
        .expect("second split commit should succeed");
    daemon.latest_seq_and_wait_idle();

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
fn daemon_trace_mirror_preserves_amend_rewrite_parity_and_records_command() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
    let control_socket = daemon_control_socket_path(&repo);
    let control_socket_str = control_socket.to_string_lossy().to_string();
    let daemon_env = [
        ("GIT_AI_DAEMON_MIRROR_TRACE", "true"),
        ("GIT_AI_DAEMON_CONTROL_SOCKET", control_socket_str.as_str()),
    ];

    fs::write(repo.path().join("trace-mirror.txt"), "line 1\n").expect("failed to write file");
    repo.git_with_env(&["add", "trace-mirror.txt"], &daemon_env, None)
        .expect("add should succeed");
    repo.git_with_env(&["commit", "-m", "initial"], &daemon_env, None)
        .expect("initial commit should succeed");

    fs::write(repo.path().join("trace-mirror.txt"), "line 1\nline 2\n")
        .expect("failed to update file");
    repo.git_with_env(&["add", "trace-mirror.txt"], &daemon_env, None)
        .expect("add before amend should succeed");
    repo.git_with_env(
        &["commit", "--amend", "-m", "initial amended"],
        &daemon_env,
        None,
    )
    .expect("amend commit should succeed");

    let latest_seq = daemon.latest_seq_and_wait_idle();
    assert!(
        latest_seq >= 3,
        "trace mirror should append start/cmd_name/exit events"
    );

    let amend_events = wait_for_rewrite_event_count(&repo, "\"commit_amend\"", 1);
    assert_eq!(
        amend_events, 1,
        "daemon trace mirroring in write mode should not duplicate commit_amend rewrite events"
    );

    let family_state = daemon.family_state_snapshot();
    let saw_commit = family_state
        .get("commands")
        .and_then(Value::as_array)
        .map(|commands| {
            commands.iter().any(|command| {
                command.get("name").and_then(Value::as_str) == Some("commit")
                    && command.get("exit_code").and_then(Value::as_i64) == Some(0)
            })
        })
        .unwrap_or(false);
    assert!(
        saw_commit,
        "daemon family state should record successful mirrored commit command"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_write_mode_applies_amend_rewrite() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];

    fs::write(repo.path().join("pure-trace.txt"), "line 1\n").expect("failed to write file");
    repo.git_og_with_env(&["add", "pure-trace.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "initial"], &env_refs)
        .expect("commit should succeed");

    fs::write(repo.path().join("pure-trace.txt"), "line 1\nline 2\n")
        .expect("failed to update file");
    repo.git_og_with_env(&["add", "pure-trace.txt"], &env_refs)
        .expect("add before amend should succeed");
    repo.git_og_with_env(&["commit", "--amend", "-m", "initial amended"], &env_refs)
        .expect("amend should succeed");

    daemon.latest_seq_and_wait_idle();

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
    let daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let default_branch = repo.current_branch();

    fs::write(repo.path().join("rebase-conflict.txt"), "base\n").expect("failed to write base");
    repo.git_og_with_env(&["add", "rebase-conflict.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
        .expect("base commit should succeed");

    repo.git_og_with_env(&["checkout", "-b", "feature"], &env_refs)
        .expect("feature branch checkout should succeed");
    fs::write(repo.path().join("rebase-conflict.txt"), "feature\n")
        .expect("failed to write feature branch change");
    repo.git_og_with_env(&["add", "rebase-conflict.txt"], &env_refs)
        .expect("feature add should succeed");
    repo.git_og_with_env(&["commit", "-m", "feature change"], &env_refs)
        .expect("feature commit should succeed");

    repo.git_og_with_env(&["checkout", default_branch.as_str()], &env_refs)
        .expect("checkout default branch should succeed");
    fs::write(repo.path().join("rebase-conflict.txt"), "main\n")
        .expect("failed to write default branch change");
    repo.git_og_with_env(&["add", "rebase-conflict.txt"], &env_refs)
        .expect("default branch add should succeed");
    repo.git_og_with_env(&["commit", "-m", "main change"], &env_refs)
        .expect("default branch commit should succeed");

    repo.git_og_with_env(&["checkout", "feature"], &env_refs)
        .expect("checkout feature should succeed");
    let rebase_conflict = repo.git_og_with_env(&["rebase", default_branch.as_str()], &env_refs);
    assert!(
        rebase_conflict.is_err(),
        "rebase should conflict for abort flow coverage"
    );
    repo.git_og_with_env(&["rebase", "--abort"], &env_refs)
        .expect("rebase abort should succeed");

    daemon.latest_seq_and_wait_idle();

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
    let daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];
    let default_branch = repo.current_branch();

    fs::write(repo.path().join("cherry-conflict.txt"), "base\n").expect("failed to write base");
    repo.git_og_with_env(&["add", "cherry-conflict.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
        .expect("base commit should succeed");

    repo.git_og_with_env(&["checkout", "-b", "topic"], &env_refs)
        .expect("topic branch checkout should succeed");
    fs::write(repo.path().join("cherry-conflict.txt"), "topic\n")
        .expect("failed to write topic branch change");
    repo.git_og_with_env(&["add", "cherry-conflict.txt"], &env_refs)
        .expect("topic add should succeed");
    repo.git_og_with_env(&["commit", "-m", "topic change"], &env_refs)
        .expect("topic commit should succeed");
    let topic_sha = repo
        .git(&["rev-parse", "topic"])
        .expect("topic rev-parse should succeed")
        .trim()
        .to_string();

    repo.git_og_with_env(&["checkout", default_branch.as_str()], &env_refs)
        .expect("checkout default branch should succeed");
    fs::write(repo.path().join("cherry-conflict.txt"), "main\n")
        .expect("failed to write default branch conflicting change");
    repo.git_og_with_env(&["add", "cherry-conflict.txt"], &env_refs)
        .expect("default branch add should succeed");
    repo.git_og_with_env(&["commit", "-m", "main change"], &env_refs)
        .expect("default branch commit should succeed");

    let cherry_pick_conflict =
        repo.git_og_with_env(&["cherry-pick", topic_sha.as_str()], &env_refs);
    assert!(
        cherry_pick_conflict.is_err(),
        "cherry-pick should conflict for abort flow coverage"
    );
    repo.git_og_with_env(&["cherry-pick", "--abort"], &env_refs)
        .expect("cherry-pick abort should succeed");

    daemon.latest_seq_and_wait_idle();

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
    let daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];

    fs::write(repo.path().join("stash-case.txt"), "base\n").expect("failed to write base");
    repo.git_og_with_env(&["add", "stash-case.txt"], &env_refs)
        .expect("base add should succeed");
    repo.git_og_with_env(&["commit", "-m", "base"], &env_refs)
        .expect("base commit should succeed");

    fs::write(repo.path().join("stash-case.txt"), "base\nchange one\n")
        .expect("failed to write stash content");
    repo.git_og_with_env(&["stash", "push", "-m", "save one"], &env_refs)
        .expect("stash push should succeed");
    repo.git_og_with_env(&["stash", "list"], &env_refs)
        .expect("stash list should succeed");
    repo.git_og_with_env(&["stash", "apply", "stash@{0}"], &env_refs)
        .expect("stash apply should succeed");

    repo.git_og_with_env(&["reset", "--hard", "HEAD"], &env_refs)
        .expect("reset hard should succeed");
    repo.git_og_with_env(&["stash", "pop", "stash@{0}"], &env_refs)
        .expect("stash pop should succeed");

    repo.git_og_with_env(&["add", "stash-case.txt"], &env_refs)
        .expect("add before commit should succeed");
    repo.git_og_with_env(&["commit", "-m", "stash pop result"], &env_refs)
        .expect("commit after stash pop should succeed");

    fs::write(repo.path().join("stash-case.txt"), "base\nchange two\n")
        .expect("failed to write second stash content");
    repo.git_og_with_env(&["stash", "push", "-m", "save two"], &env_refs)
        .expect("second stash push should succeed");
    repo.git_og_with_env(&["stash", "drop", "stash@{0}"], &env_refs)
        .expect("stash drop should succeed");

    daemon.latest_seq_and_wait_idle();

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
fn daemon_pure_trace_socket_reset_modes_emit_reset_kinds() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];

    fs::write(repo.path().join("reset-case.txt"), "line 1\n").expect("failed to write file");
    repo.git_og_with_env(&["add", "reset-case.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "c1"], &env_refs)
        .expect("c1 should succeed");

    fs::write(repo.path().join("reset-case.txt"), "line 1\nline 2\n")
        .expect("failed to write c2 content");
    repo.git_og_with_env(&["add", "reset-case.txt"], &env_refs)
        .expect("add c2 should succeed");
    repo.git_og_with_env(&["commit", "-m", "c2"], &env_refs)
        .expect("c2 should succeed");

    fs::write(
        repo.path().join("reset-case.txt"),
        "line 1\nline 2\nline 3\n",
    )
    .expect("failed to write c3 content");
    repo.git_og_with_env(&["add", "reset-case.txt"], &env_refs)
        .expect("add c3 should succeed");
    repo.git_og_with_env(&["commit", "-m", "c3"], &env_refs)
        .expect("c3 should succeed");

    fs::write(
        repo.path().join("reset-case.txt"),
        "line 1\nline 2\nline 3\nline 4\n",
    )
    .expect("failed to write c4 content");
    repo.git_og_with_env(&["add", "reset-case.txt"], &env_refs)
        .expect("add c4 should succeed");
    repo.git_og_with_env(&["commit", "-m", "c4"], &env_refs)
        .expect("c4 should succeed");

    repo.git_og_with_env(&["reset", "--soft", "HEAD~1"], &env_refs)
        .expect("soft reset should succeed");
    repo.git_og_with_env(&["reset", "--mixed", "HEAD~1"], &env_refs)
        .expect("mixed reset should succeed");
    repo.git_og_with_env(&["reset", "--hard", "HEAD~1"], &env_refs)
        .expect("hard reset should succeed");

    daemon.latest_seq_and_wait_idle();
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
fn daemon_pure_trace_socket_rebase_continue_emits_complete_event() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
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

    fs::write(repo.path().join("rebase-continue.txt"), "resolved\n")
        .expect("failed to write resolved content");
    repo.git_og_with_env(&["add", "rebase-continue.txt"], &env_refs)
        .expect("add resolved should succeed");
    repo.git_og_with_env(&["rebase", "--continue"], &env_refs)
        .expect("rebase continue should succeed");

    daemon.latest_seq_and_wait_idle();

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
fn daemon_pure_trace_socket_cherry_pick_continue_emits_complete_event() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
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

    fs::write(repo.path().join("cherry-continue.txt"), "resolved\n")
        .expect("failed to write resolved cherry content");
    repo.git_og_with_env(&["add", "cherry-continue.txt"], &env_refs)
        .expect("add resolved cherry content should succeed");
    repo.git_og_with_env(&["cherry-pick", "--continue"], &env_refs)
        .expect("cherry-pick continue should succeed");

    daemon.latest_seq_and_wait_idle();

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
    let daemon = DaemonGuard::start(&repo);
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

    daemon.latest_seq_and_wait_idle();

    let family_state = daemon.family_state_snapshot();
    let commands = family_state
        .get("commands")
        .and_then(Value::as_array)
        .expect("family state should contain command history");
    let saw_switch_success = commands.iter().any(|command| {
        command.get("name").and_then(Value::as_str) == Some("switch")
            && command.get("exit_code").and_then(Value::as_i64) == Some(0)
    });
    let saw_switch_failure = commands.iter().any(|command| {
        command.get("name").and_then(Value::as_str) == Some("switch")
            && command
                .get("exit_code")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                != 0
    });
    assert!(saw_switch_success, "switch success should be tracked");
    assert!(saw_switch_failure, "switch failure should be tracked");
}

#[test]
#[serial]
fn daemon_pure_trace_socket_checkout_tracks_success_failure_and_new_branch() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
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

    daemon.latest_seq_and_wait_idle();

    let family_state = daemon.family_state_snapshot();
    let commands = family_state
        .get("commands")
        .and_then(Value::as_array)
        .expect("family state should contain command history");
    let saw_checkout_success = commands.iter().any(|command| {
        command.get("name").and_then(Value::as_str) == Some("checkout")
            && command.get("exit_code").and_then(Value::as_i64) == Some(0)
    });
    let saw_checkout_failure = commands.iter().any(|command| {
        command.get("name").and_then(Value::as_str) == Some("checkout")
            && command
                .get("exit_code")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                != 0
    });
    let saw_checkout_new_branch = commands.iter().any(|command| {
        command.get("name").and_then(Value::as_str) == Some("checkout")
            && command
                .get("argv")
                .and_then(Value::as_array)
                .map(|argv| argv.iter().any(|arg| arg.as_str() == Some("-b")))
                .unwrap_or(false)
    });
    assert!(saw_checkout_success, "checkout success should be tracked");
    assert!(saw_checkout_failure, "checkout failure should be tracked");
    assert!(
        saw_checkout_new_branch,
        "checkout new branch (-b) flow should be tracked"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_pull_fast_forward_tracks_pull_command_and_ref_reconcile() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
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

    daemon.latest_seq_and_wait_idle();

    let family_state = daemon.family_state_snapshot();
    let commands = family_state
        .get("commands")
        .and_then(Value::as_array)
        .expect("family state should contain command history");
    let saw_pull_success = commands.iter().any(|command| {
        command.get("name").and_then(Value::as_str) == Some("pull")
            && command.get("exit_code").and_then(Value::as_i64) == Some(0)
    });
    assert!(saw_pull_success, "pull success should be tracked");

    let saw_pull_ref_reconcile = family_state
        .get("rewrite_events")
        .and_then(Value::as_array)
        .map(|events| {
            events.iter().any(|event| {
                event
                    .get("ref_reconcile")
                    .and_then(|ref_reconcile| ref_reconcile.get("command"))
                    .and_then(Value::as_str)
                    == Some("pull")
            })
        })
        .unwrap_or(false);
    assert!(
        saw_pull_ref_reconcile,
        "pull fast-forward should record pull ref_reconcile rewrite signal"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_pull_rebase_tracks_pull_and_rebase_completion() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
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

    daemon.latest_seq_and_wait_idle();

    let family_state = daemon.family_state_snapshot();
    let commands = family_state
        .get("commands")
        .and_then(Value::as_array)
        .expect("family state should contain command history");
    let saw_pull_rebase_success = commands.iter().any(|command| {
        command.get("name").and_then(Value::as_str) == Some("pull")
            && command.get("exit_code").and_then(Value::as_i64) == Some(0)
            && command
                .get("argv")
                .and_then(Value::as_array)
                .map(|argv| argv.iter().any(|arg| arg.as_str() == Some("--rebase")))
                .unwrap_or(false)
    });
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
    let daemon = DaemonGuard::start(&repo);
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

    daemon.latest_seq_and_wait_idle();

    let local_contents = fs::read_to_string(repo.path().join("autostash-local.txt"))
        .expect("local file should remain readable");
    assert!(
        local_contents.contains("local dirty change"),
        "autostash pull should preserve local dirty change content"
    );

    let family_state = daemon.family_state_snapshot();
    let commands = family_state
        .get("commands")
        .and_then(Value::as_array)
        .expect("family state should contain command history");
    let saw_pull_autostash_success = commands.iter().any(|command| {
        command.get("name").and_then(Value::as_str) == Some("pull")
            && command.get("exit_code").and_then(Value::as_i64) == Some(0)
            && command
                .get("argv")
                .and_then(Value::as_array)
                .map(|argv| {
                    let has_rebase = argv.iter().any(|arg| arg.as_str() == Some("--rebase"));
                    let has_autostash = argv.iter().any(|arg| arg.as_str() == Some("--autostash"));
                    has_rebase && has_autostash
                })
                .unwrap_or(false)
    });
    assert!(
        saw_pull_autostash_success,
        "pull --rebase --autostash success should be tracked"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_high_throughput_ai_commit_burst_preserves_exact_blame() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
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

    daemon.latest_seq_and_wait_idle();
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
    let daemon = DaemonGuard::start(&repo);
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

    let file_count = 10usize;
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

    daemon.latest_seq_and_wait_idle();

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
    let daemon = DaemonGuard::start(&repo);
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];

    let harness = WorkdirRaceHarness::new(&repo, trace_socket.clone());
    let workdir = repo.path().to_path_buf();

    let file_count = 12usize;
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

    daemon.latest_seq_and_wait_idle();

    for (file_rel, line) in expected {
        let mut file = repo.filename(file_rel.as_str());
        file.assert_lines_and_blame(lines![line.ai()]);
    }
}

#[test]
#[serial]
fn daemon_pure_trace_socket_parallel_worktree_streams_preserve_exact_line_attribution() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo);
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

    let file_count = 8usize;

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

    daemon.latest_seq_and_wait_idle();

    let family_state = daemon.family_state_snapshot();
    let commit_events = family_state
        .get("rewrite_events")
        .and_then(Value::as_array)
        .expect("family state should contain rewrite events")
        .iter()
        .filter_map(|event| {
            event
                .get("commit")
                .and_then(Value::as_object)
                .and_then(|commit| commit.get("commit_sha"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        commit_events.len(),
        2,
        "parallel worktree commits should emit two commit rewrite events: {:?}",
        commit_events
    );
    assert_ne!(
        commit_events[0], commit_events[1],
        "parallel worktree commits should preserve distinct commit SHAs: {:?}",
        commit_events
    );

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
