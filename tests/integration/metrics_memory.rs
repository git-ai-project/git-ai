use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::metrics::attrs::attr_pos;
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::types::MetricEventId;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const MAX_METRIC_EVENT_JSON_BYTES: usize = 2 * 1024 * 1024;

fn codex_checkpoint(repo: &TestRepo, file_path: &Path, hook_event_name: &str, tool_use_id: &str) {
    let hook_input = json!({
        "session_id": "metrics-memory-session",
        "cwd": repo.canonical_path().to_string_lossy(),
        "hook_event_name": hook_event_name,
        "tool_name": "apply_patch",
        "tool_use_id": tool_use_id,
        "model": "gpt-5",
        "tool_input": {
            "patch": format!("*** Update File: {}\n", file_path.to_string_lossy())
        },
    })
    .to_string();

    repo.git_ai(&["checkpoint", "codex", "--hook-input", &hook_input])
        .expect("codex checkpoint should succeed");
}

fn commit_sha(event: &git_ai::metrics::MetricEvent) -> Option<&str> {
    event
        .attrs
        .get(&attr_pos::COMMIT_SHA.to_string())
        .and_then(|value| value.as_str())
}

#[test]
fn oversized_commit_metric_is_not_retained_and_following_metric_persists() {
    let metrics_dir = tempfile::tempdir().unwrap();
    let metrics_db_path = metrics_dir.path().join("metrics.db");
    let metrics_db_path_str = metrics_db_path.to_string_lossy().to_string();
    let repo = TestRepo::new_with_daemon_env(&[(
        "GIT_AI_TEST_METRICS_DB_PATH",
        metrics_db_path_str.as_str(),
    )]);

    let file_path = repo.path().join("generated.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("generated.txt");
    file.assert_committed_lines(lines!["base".unattributed_human()]);

    codex_checkpoint(&repo, &file_path, "PreToolUse", "oversized-metric-edit");
    fs::write(&file_path, "base\nfirst ai line\n").unwrap();
    codex_checkpoint(&repo, &file_path, "PostToolUse", "oversized-metric-edit");
    repo.git(&["add", "-A"]).unwrap();

    let message_path = metrics_dir.path().join("oversized-commit-message.txt");
    fs::write(
        &message_path,
        format!(
            "Oversized commit metric\n\n{}\n",
            "x".repeat(3 * 1024 * 1024)
        ),
    )
    .unwrap();
    repo.git(&[
        "commit",
        "-F",
        message_path.to_str().expect("message path should be UTF-8"),
    ])
    .unwrap();
    repo.sync_daemon();
    let oversized_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    file.assert_committed_lines(lines!["base".unattributed_human(), "first ai line".ai()]);

    codex_checkpoint(&repo, &file_path, "PreToolUse", "normal-metric-edit");
    fs::write(&file_path, "base\nfirst ai line\nsecond ai line\n").unwrap();
    codex_checkpoint(&repo, &file_path, "PostToolUse", "normal-metric-edit");
    let normal_commit = repo.stage_all_and_commit("Normal commit metric").unwrap();
    file.assert_committed_lines(lines![
        "base".unattributed_human(),
        "first ai line".ai(),
        "second ai line".ai(),
    ]);

    let deadline = Instant::now() + Duration::from_secs(10);
    let settle_time = Duration::from_secs(1);
    let mut normal_seen_at = None;
    loop {
        let db = MetricsDatabase::open_at_path(&metrics_db_path).unwrap();
        let events = db
            .get_metric_history(0, None, &[MetricEventId::Committed as u16])
            .unwrap()
            .into_iter()
            .map(|record| record.event)
            .collect::<Vec<_>>();

        assert!(
            events.iter().all(|event| {
                serde_json::to_vec(event)
                    .is_ok_and(|json| json.len() <= MAX_METRIC_EVENT_JSON_BYTES)
            }),
            "persisted metric exceeded the per-event byte limit"
        );
        assert!(
            events
                .iter()
                .all(|event| commit_sha(event) != Some(oversized_commit.as_str())),
            "oversized committed metric was retained"
        );

        if events
            .iter()
            .any(|event| commit_sha(event) == Some(normal_commit.commit_sha.as_str()))
        {
            let seen_at = normal_seen_at.get_or_insert_with(Instant::now);
            if seen_at.elapsed() >= settle_time {
                break;
            }
        }

        assert!(
            Instant::now() < deadline,
            "normal metric after oversized metric was not persisted"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    repo.git_ai(&["status", "--json"])
        .expect("daemon should remain responsive after oversized metric");
}
