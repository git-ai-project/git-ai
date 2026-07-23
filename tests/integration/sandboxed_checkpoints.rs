use crate::repos::test_repo::{DaemonTestScope, TestRepo};
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::{CheckpointValues, PosEncoded};
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

    let deadline = Instant::now() + Duration::from_secs(10);
    let event = loop {
        let db = MetricsDatabase::open_at_path(&metrics_path).unwrap();
        let records = db.get_metric_history(0, None, &[4]).unwrap();
        if let Some(record) = records.into_iter().find(|record| {
            let values = CheckpointValues::from_sparse(&record.event.values);
            values.edit_kind.as_ref().and_then(|value| value.as_deref())
                == Some("file_edit_sandboxed")
        }) {
            break record.event;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not import checkpoint"
        );
        thread::sleep(Duration::from_millis(25));
    };

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
