use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{DaemonTestScope, TestRepo};
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::types::MetricEventId;
use git_ai::metrics::{CheckpointValues, MetricEvent, PosEncoded};
use git_ai::sandboxed_checkpoints::{
    SandboxedCheckpointKind, SandboxedCheckpointPhase, SandboxedCheckpointRecord,
};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

fn codex_hook_input(repo: &TestRepo, session_id: &str) -> String {
    json!({
        "session_id": session_id,
        "cwd": repo.canonical_path(),
        "hook_event_name": "PostToolUse",
        "tool_name": "apply_patch",
        "tool_use_id": "tool-use-1",
        "model": "gpt-5",
        "tool_input": {
            "patch": "*** Update File: generated.txt\n"
        }
    })
    .to_string()
}

fn checkpoint_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut paths = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("ckpt"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn wait_for_checkpoint_metrics(
    metrics_path: &Path,
    edit_kind: &str,
    expected_count: usize,
) -> Vec<MetricEvent> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let db = MetricsDatabase::open_at_path(metrics_path).unwrap();
        let records = db
            .get_metric_history(0, None, &[MetricEventId::Checkpoint as u16])
            .unwrap();
        let events = records
            .into_iter()
            .map(|record| record.event)
            .filter(|event| {
                let values = CheckpointValues::from_sparse(&event.values);
                values.edit_kind.as_ref().and_then(|value| value.as_deref()) == Some(edit_kind)
            })
            .collect::<Vec<_>>();
        if events.len() >= expected_count {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "expected {expected_count} {edit_kind} checkpoint metrics, found {}",
            events.len()
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn sandbox_markers_capture_metadata_without_normal_checkpointing() {
    for env_var in [
        "CURSOR_SANDBOX",
        "SANDBOX_RUNTIME",
        "CODEX_SANDBOX",
        "CODEX_SANDBOX_NETWORK_DISABLED",
    ] {
        let repo = TestRepo::new_with_daemon_scope(DaemonTestScope::NoDaemon);
        let spool = tempfile::tempdir().unwrap();
        let hook_input = codex_hook_input(&repo, &format!("session-{env_var}"));
        let unavailable_home = spool.path().join("unavailable-home");
        let unavailable_codex_home = spool.path().join("unavailable-codex-home");

        repo.git_ai_with_env(
            &["checkpoint", "codex", "--hook-input", &hook_input],
            &[
                (env_var, ""),
                (
                    "GIT_AI_TEST_SANDBOX_CHECKPOINT_DIR",
                    spool.path().to_str().unwrap(),
                ),
                ("GIT_CONFIG_GLOBAL", "/definitely/unavailable/gitconfig"),
                ("HOME", unavailable_home.to_str().unwrap()),
                ("CODEX_HOME", unavailable_codex_home.to_str().unwrap()),
            ],
        )
        .expect("sandboxed checkpoint should remain best-effort successful");

        let files = checkpoint_files(spool.path());
        assert_eq!(files.len(), 1, "{env_var} should create one record");
        let raw = fs::read_to_string(&files[0]).unwrap();
        assert!(!raw.contains("tool_input"));
        assert!(!raw.contains("content"));
        assert!(!raw.contains("command"));

        let record: SandboxedCheckpointRecord = serde_json::from_str(&raw).unwrap();
        assert_eq!(record.kind, SandboxedCheckpointKind::FileEdit);
        assert_eq!(record.phase, SandboxedCheckpointPhase::Post);
        assert_eq!(record.agent_id.tool, "codex");
        assert_eq!(record.agent_id.model, "gpt-5");
        assert_eq!(record.tool_use_id.as_deref(), Some("tool-use-1"));
        assert_eq!(
            record.file_paths,
            vec![repo.canonical_path().join("generated.txt")]
        );
        assert!(
            !repo.path().join(".git/ai/working_logs").exists(),
            "fallback must not create working-log checkpoints"
        );
    }
}

#[test]
fn unavailable_daemon_uses_the_same_metadata_fallback() {
    let repo = TestRepo::new_with_daemon_scope(DaemonTestScope::NoDaemon);
    let spool = tempfile::tempdir().unwrap();
    let hook_input = codex_hook_input(&repo, "daemon-unavailable-session");

    repo.git_ai_with_env(
        &["checkpoint", "codex", "--hook-input", &hook_input],
        &[(
            "GIT_AI_TEST_SANDBOX_CHECKPOINT_DIR",
            spool.path().to_str().unwrap(),
        )],
    )
    .expect("unavailable daemon should use the best-effort fallback");

    assert_eq!(checkpoint_files(spool.path()).len(), 1);
}

#[test]
fn daemon_imports_sandboxed_checkpoint_into_metrics_database() {
    let spool = tempfile::tempdir().unwrap();
    let metrics = tempfile::tempdir().unwrap();
    let metrics_path = metrics.path().join("metrics.db");
    let metrics_path_string = metrics_path.to_string_lossy().to_string();
    let spool_path_string = spool.path().to_string_lossy().to_string();
    let repo = TestRepo::new_with_daemon_env(&[
        (
            "GIT_AI_TEST_SANDBOX_CHECKPOINT_DIR",
            spool_path_string.as_str(),
        ),
        ("GIT_AI_TEST_SANDBOX_CHECKPOINT_POLL_MS", "10"),
        ("GIT_AI_TEST_METRICS_DB_PATH", metrics_path_string.as_str()),
    ]);
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/sandboxed-checkpoints.git",
    ])
    .unwrap();
    let hook_input = codex_hook_input(&repo, "imported-session");

    repo.git_ai_with_env(
        &["checkpoint", "codex", "--hook-input", &hook_input],
        &[
            ("CODEX_SANDBOX", "1"),
            (
                "GIT_AI_TEST_SANDBOX_CHECKPOINT_DIR",
                spool_path_string.as_str(),
            ),
        ],
    )
    .unwrap();

    let event = wait_for_checkpoint_metrics(&metrics_path, "file_edit_sandboxed", 1)
        .into_iter()
        .next()
        .unwrap();

    let values = CheckpointValues::from_sparse(&event.values);
    assert_eq!(
        values.checkpoint_type,
        Some(Some("sandboxed_fallback".to_string()))
    );
    assert_eq!(
        values.file_path,
        Some(Some(
            repo.canonical_path()
                .join("generated.txt")
                .to_string_lossy()
                .to_string()
        ))
    );
    assert!(checkpoint_files(spool.path()).is_empty());

    let healthy_hook_input = codex_hook_input(&repo, "healthy-daemon-session");
    repo.git_ai_with_env(
        &["checkpoint", "codex", "--hook-input", &healthy_hook_input],
        &[(
            "GIT_AI_TEST_SANDBOX_CHECKPOINT_DIR",
            spool_path_string.as_str(),
        )],
    )
    .unwrap();
    assert!(
        checkpoint_files(spool.path()).is_empty(),
        "a healthy daemon should use the normal checkpoint flow"
    );
}

#[test]
fn sandboxed_file_edit_checkpoint_recovers_ai_attribution() {
    let spool = tempfile::tempdir().unwrap();
    let metrics = tempfile::tempdir().unwrap();
    let metrics_path = metrics.path().join("metrics.db");
    let metrics_path_string = metrics_path.to_string_lossy().to_string();
    let spool_path_string = spool.path().to_string_lossy().to_string();
    let repo = TestRepo::new_with_daemon_env(&[
        (
            "GIT_AI_TEST_SANDBOX_CHECKPOINT_DIR",
            spool_path_string.as_str(),
        ),
        ("GIT_AI_TEST_SANDBOX_CHECKPOINT_POLL_MS", "10"),
        ("GIT_AI_TEST_METRICS_DB_PATH", metrics_path_string.as_str()),
    ]);
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/sandboxed-file-recovery.git",
    ])
    .unwrap();

    let file_path = repo.path().join("generated.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    fs::write(&file_path, "base\nAI file edit\n").unwrap();

    let hook_input = codex_hook_input(&repo, "sandboxed-file-session");
    repo.git_ai_with_env(
        &["checkpoint", "codex", "--hook-input", &hook_input],
        &[
            ("CODEX_SANDBOX", "1"),
            (
                "GIT_AI_TEST_SANDBOX_CHECKPOINT_DIR",
                spool_path_string.as_str(),
            ),
        ],
    )
    .unwrap();
    wait_for_checkpoint_metrics(&metrics_path, "file_edit_sandboxed", 1);

    let commit = repo.stage_all_and_commit("Sandboxed file edit").unwrap();
    let mut file = repo.filename("generated.txt");
    file.assert_committed_lines(crate::lines![
        "base".unattributed_human(),
        "AI file edit".ai(),
    ]);
    assert!(
        commit
            .authorship_log
            .metadata
            .sessions
            .values()
            .any(|session| {
                session.agent_id.tool == "codex" && session.agent_id.id == "sandboxed-file-session"
            })
    );
    wait_for_checkpoint_metrics(
        &metrics_path,
        "attribution_recovery_sandboxed_checkpoint",
        1,
    );
}

#[test]
fn sandboxed_bash_checkpoints_recover_ai_attribution() {
    let spool = tempfile::tempdir().unwrap();
    let metrics = tempfile::tempdir().unwrap();
    let metrics_path = metrics.path().join("metrics.db");
    let metrics_path_string = metrics_path.to_string_lossy().to_string();
    let spool_path_string = spool.path().to_string_lossy().to_string();
    let repo = TestRepo::new_with_daemon_env(&[
        (
            "GIT_AI_TEST_SANDBOX_CHECKPOINT_DIR",
            spool_path_string.as_str(),
        ),
        ("GIT_AI_TEST_SANDBOX_CHECKPOINT_POLL_MS", "10"),
        ("GIT_AI_TEST_METRICS_DB_PATH", metrics_path_string.as_str()),
    ]);
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/sandboxed-bash-recovery.git",
    ])
    .unwrap();

    let file_path = repo.path().join("generated.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    for hook_event_name in ["PreToolUse", "PostToolUse"] {
        if hook_event_name == "PostToolUse" {
            fs::write(&file_path, "base\nAI bash edit\n").unwrap();
        }
        let hook_input = json!({
            "session_id": "sandboxed-bash-session",
            "cwd": repo.canonical_path(),
            "hook_event_name": hook_event_name,
            "tool_name": "shell_command",
            "tool_use_id": "bash-tool-use-1",
            "model": "gpt-5",
            "tool_input": { "command": "printf 'AI bash edit\\n' >> generated.txt" }
        })
        .to_string();
        repo.git_ai_with_env(
            &["checkpoint", "codex", "--hook-input", &hook_input],
            &[
                ("CODEX_SANDBOX", "1"),
                (
                    "GIT_AI_TEST_SANDBOX_CHECKPOINT_DIR",
                    spool_path_string.as_str(),
                ),
            ],
        )
        .unwrap();
    }
    wait_for_checkpoint_metrics(&metrics_path, "bash_sandboxed", 2);

    let commit = repo.stage_all_and_commit("Sandboxed bash edit").unwrap();
    let mut file = repo.filename("generated.txt");
    file.assert_committed_lines(crate::lines![
        "base".unattributed_human(),
        "AI bash edit".ai(),
    ]);
    assert!(
        commit
            .authorship_log
            .metadata
            .sessions
            .values()
            .any(|session| {
                session.agent_id.tool == "codex" && session.agent_id.id == "sandboxed-bash-session"
            })
    );
    wait_for_checkpoint_metrics(
        &metrics_path,
        "attribution_recovery_sandboxed_checkpoint",
        1,
    );
}
