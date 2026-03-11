#[macro_use]
mod repos;

use git_ai::authorship::working_log::CheckpointKind;
use git_ai::daemon::{ControlRequest, ControlResponse, send_control_request};
use repos::test_repo::{GitTestMode, TestRepo, get_binary_path, real_git_executable};
use serde_json::Value;
use serial_test::serial;
use std::fs;
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

fn family_state_path(repo: &TestRepo) -> PathBuf {
    git_common_dir(repo)
        .join("ai")
        .join("state")
        .join("daemon-v1")
        .join("family_state.json")
}

fn daemon_control_socket_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("control.sock")
}

fn daemon_trace_socket_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("trace2.sock")
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
    fn start(repo: &TestRepo, mode: &str) -> Self {
        let mut command = Command::new(get_binary_path());
        command
            .arg("daemon")
            .arg("start")
            .arg("--mode")
            .arg(mode)
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
        let mut last_latest_seq = 0_u64;
        let mut stable_idle_polls = 0_u8;

        for _ in 0..200 {
            let status = self.request(ControlRequest::StatusFamily {
                repo_working_dir: self.repo_working_dir.clone(),
            });
            assert!(status.ok, "status request should succeed");
            let latest_seq = status
                .data
                .as_ref()
                .and_then(|v| v.get("latest_seq"))
                .and_then(Value::as_u64)
                .unwrap_or(0);

            if latest_seq > 0 {
                let barrier = self.request(ControlRequest::BarrierAppliedThroughSeq {
                    repo_working_dir: self.repo_working_dir.clone(),
                    seq: latest_seq,
                });
                assert!(barrier.ok, "barrier request should succeed");
            }

            let settled = self.request(ControlRequest::StatusFamily {
                repo_working_dir: self.repo_working_dir.clone(),
            });
            assert!(settled.ok, "settled status request should succeed");
            let settled_latest = settled
                .data
                .as_ref()
                .and_then(|v| v.get("latest_seq"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let settled_backlog = settled
                .data
                .as_ref()
                .and_then(|v| v.get("backlog"))
                .and_then(Value::as_u64)
                .unwrap_or(0);

            if settled_backlog == 0 && settled_latest == last_latest_seq {
                stable_idle_polls = stable_idle_polls.saturating_add(1);
                if stable_idle_polls >= 2 {
                    return settled_latest;
                }
            } else {
                stable_idle_polls = 0;
            }
            last_latest_seq = settled_latest;
            thread::sleep(Duration::from_millis(25));
        }

        last_latest_seq
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

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[test]
#[serial]
fn checkpoint_delegate_falls_back_when_daemon_is_unavailable() {
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
    .expect("checkpoint should fall back to local mode");

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    assert!(
        checkpoints
            .iter()
            .any(|checkpoint| checkpoint.kind == CheckpointKind::AiAgent),
        "local fallback should write ai_agent checkpoint when daemon is unavailable"
    );
}

#[test]
#[serial]
fn daemon_write_mode_applies_delegated_checkpoint_and_updates_state() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo, "write");

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

    let family_state_raw = fs::read_to_string(family_state_path(&repo))
        .expect("family state should exist after delegated checkpoint");
    let family_state: Value =
        serde_json::from_str(&family_state_raw).expect("family state should be valid json");
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
fn daemon_shadow_mode_tracks_checkpoint_without_applying_side_effects() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo, "shadow");

    fs::write(repo.path().join("delegate-shadow.txt"), "base\n").expect("failed to write base");
    repo.git(&["add", "delegate-shadow.txt"])
        .expect("add should succeed");
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    fs::write(
        repo.path().join("delegate-shadow.txt"),
        "base\ntracked in shadow mode only\n",
    )
    .expect("failed to write updated file");

    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "delegate-shadow.txt"],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("shadow delegated checkpoint should succeed");

    daemon.latest_seq_and_wait_idle();

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    assert!(
        checkpoints.is_empty(),
        "shadow-mode daemon should not apply checkpoint side effects"
    );

    let family_state_raw = fs::read_to_string(family_state_path(&repo))
        .expect("family state should exist after shadow delegated checkpoint");
    let family_state: Value =
        serde_json::from_str(&family_state_raw).expect("family state should be valid json");
    let checkpoints_map = family_state
        .get("checkpoints")
        .and_then(Value::as_object)
        .expect("family state should contain checkpoints map");
    assert!(
        !checkpoints_map.is_empty(),
        "shadow-mode daemon should still track checkpoint summaries in state"
    );
}

#[test]
#[serial]
fn daemon_trace_mirror_preserves_amend_rewrite_parity_and_records_command() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo, "write");
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

    let rewrite_log_path = git_common_dir(&repo).join("ai").join("rewrite_log");
    let rewrite_log =
        fs::read_to_string(&rewrite_log_path).expect("rewrite log should exist after amend");
    let amend_events = rewrite_log
        .lines()
        .filter(|line| line.contains("\"commit_amend\""))
        .count();
    assert_eq!(
        amend_events, 1,
        "daemon trace mirroring in write mode should not duplicate commit_amend rewrite events"
    );

    let family_state_raw = fs::read_to_string(family_state_path(&repo))
        .expect("family state should exist after mirrored trace events");
    let family_state: Value =
        serde_json::from_str(&family_state_raw).expect("family state should be valid json");
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
    let daemon = DaemonGuard::start(&repo, "write");
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

    let rewrite_log_path = git_common_dir(&repo).join("ai").join("rewrite_log");
    let rewrite_log =
        fs::read_to_string(&rewrite_log_path).expect("rewrite log should exist after amend");
    let amend_events = rewrite_log
        .lines()
        .filter(|line| line.contains("\"commit_amend\""))
        .count();
    assert_eq!(
        amend_events, 1,
        "pure trace socket mode should emit exactly one commit_amend rewrite event"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_shadow_mode_tracks_without_writes() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo, "shadow");
    let trace_socket = daemon_trace_socket_path(&repo);
    let env = git_trace_env(&trace_socket);
    let env_refs = [(env[0].0, env[0].1.as_str()), (env[1].0, env[1].1.as_str())];

    fs::write(repo.path().join("pure-shadow.txt"), "line 1\n").expect("failed to write file");
    repo.git_og_with_env(&["add", "pure-shadow.txt"], &env_refs)
        .expect("add should succeed");
    repo.git_og_with_env(&["commit", "-m", "shadow commit"], &env_refs)
        .expect("commit should succeed");

    daemon.latest_seq_and_wait_idle();

    let rewrite_log_path = git_common_dir(&repo).join("ai").join("rewrite_log");
    assert!(
        !rewrite_log_path.exists()
            || fs::read_to_string(&rewrite_log_path)
                .unwrap_or_default()
                .is_empty(),
        "shadow mode should not apply rewrite side effects from pure trace socket events"
    );

    let family_state_raw = fs::read_to_string(family_state_path(&repo))
        .expect("family state should exist after pure trace events");
    let family_state: Value =
        serde_json::from_str(&family_state_raw).expect("family state should be valid json");
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
        "shadow mode should still track commands from pure trace socket events"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_rebase_abort_emits_abort_event() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo, "write");
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
    let daemon = DaemonGuard::start(&repo, "write");
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
    let daemon = DaemonGuard::start(&repo, "write");
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
    let daemon = DaemonGuard::start(&repo, "write");
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
            kind
        );
    }
}

#[test]
#[serial]
fn daemon_pure_trace_socket_rebase_continue_emits_complete_event() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo, "write");
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
    let daemon = DaemonGuard::start(&repo, "write");
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
    let daemon = DaemonGuard::start(&repo, "write");
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

    let family_state_raw = fs::read_to_string(family_state_path(&repo))
        .expect("family state should exist after switch operations");
    let family_state: Value =
        serde_json::from_str(&family_state_raw).expect("family state should be valid json");
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
    let daemon = DaemonGuard::start(&repo, "write");
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

    let family_state_raw = fs::read_to_string(family_state_path(&repo))
        .expect("family state should exist after checkout operations");
    let family_state: Value =
        serde_json::from_str(&family_state_raw).expect("family state should be valid json");
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
    let daemon = DaemonGuard::start(&repo, "write");
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

    run_git(&["clone", bare_remote_str.as_str(), remote_clone_str.as_str()]);
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
        default_branch.as_str(),
    ]);

    repo.git_og_with_env(
        &["pull", "--ff-only", "origin", default_branch.as_str()],
        &env_refs,
    )
    .expect("fast-forward pull should succeed");

    daemon.latest_seq_and_wait_idle();

    let family_state_raw = fs::read_to_string(family_state_path(&repo))
        .expect("family state should exist after pull operation");
    let family_state: Value =
        serde_json::from_str(&family_state_raw).expect("family state should be valid json");
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
    let daemon = DaemonGuard::start(&repo, "write");
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

    run_git(&["clone", bare_remote_str.as_str(), remote_clone_str.as_str()]);
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
        default_branch.as_str(),
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

    let family_state_raw = fs::read_to_string(family_state_path(&repo))
        .expect("family state should exist after pull --rebase operation");
    let family_state: Value =
        serde_json::from_str(&family_state_raw).expect("family state should be valid json");
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

    let rewrite_log_path = git_common_dir(&repo).join("ai").join("rewrite_log");
    let rewrite_log = fs::read_to_string(&rewrite_log_path)
        .expect("rewrite log should exist after pull --rebase");
    assert!(
        rewrite_log.contains("\"rebase_complete\""),
        "pull --rebase should result in a rebase_complete rewrite signal"
    );
}

#[test]
#[serial]
fn daemon_pure_trace_socket_pull_autostash_preserves_local_changes_and_tracks_command() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo, "write");
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

    run_git(&["clone", bare_remote_str.as_str(), remote_clone_str.as_str()]);
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
        default_branch.as_str(),
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

    let family_state_raw = fs::read_to_string(family_state_path(&repo))
        .expect("family state should exist after pull --autostash operation");
    let family_state: Value =
        serde_json::from_str(&family_state_raw).expect("family state should be valid json");
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
