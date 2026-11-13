use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{DaemonTestScope, TestRepo};
use crate::test_utils::{extract_json_object, isolated_metrics_db_path};
use chrono::NaiveDate;
use serde_json::Value;
use std::time::{Duration, Instant};

fn seed_ai_commit(repo: &TestRepo) {
    let mut file = repo.filename("app.rs");
    file.set_contents(crate::lines!["fn main() {}", "let answer = 42;".ai()]);
    repo.stage_all_and_commit("AI commit")
        .expect("AI commit should succeed");
    file.assert_committed_lines(crate::lines![
        "fn main() {}".human(),
        "let answer = 42;".ai()
    ]);
}

/// Run `git-ai usage` with the metrics DB pointed at the daemon's isolated path,
/// retrying until the just-committed activity is persisted or the deadline passes.
fn usage_json(repo: &TestRepo, metrics_db_path: &str, extra_args: &[&str]) -> Value {
    let mut args = vec!["usage"];
    args.extend_from_slice(extra_args);
    args.push("--json");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        repo.sync_daemon_force();
        let result =
            repo.git_ai_with_env(&args, &[("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path)]);
        if let Ok(output) = &result {
            let json = extract_json_object(output);
            if let Ok(value) = serde_json::from_str::<Value>(&json) {
                return value;
            }
        }

        if Instant::now() >= deadline {
            panic!("usage {args:?} did not return activity data: {result:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The window spans `calendar_start` to `calendar_end` as local dates. A fixed
/// `days * 86400` second subtraction lands on the same wall-clock time N days back,
/// so the span is N days except within an hour of midnight on a DST-transition day,
/// where it can shift by one. Callers assert a `[N - 1, N]` range to tolerate that.
fn window_span_days(value: &Value) -> i64 {
    let parse = |key: &str| {
        let text = value[key].as_str().expect("date field should be a string");
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("date field should parse")
    };
    (parse("calendar_end") - parse("calendar_start")).num_days()
}

fn assert_window(value: &Value, expected_label: &str, expected_days: i64) {
    assert_eq!(value["period_label"].as_str().unwrap(), expected_label);
    let span = window_span_days(value);
    assert!(
        (expected_days - 1..=expected_days).contains(&span),
        "expected a {expected_days}-day window (tolerating one DST day), got span {span}"
    );
}

#[test]
fn usage_period_valid_tokens_report_their_label_and_window() {
    let (_metrics_db_dir, metrics_db_path) = isolated_metrics_db_path();
    let repo =
        TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path.as_str())]);
    seed_ai_commit(&repo);
    let cases = [
        ("1d", "last 24 hours", 1),
        ("3d", "last 3 days", 3),
        ("7d", "last 7 days", 7),
        ("30d", "last 30 days", 30),
    ];

    for (token, expected_label, expected_days) in cases {
        let value = usage_json(&repo, &metrics_db_path, &["--period", token]);
        assert_window(&value, expected_label, expected_days);
    }
}

#[test]
fn usage_period_accepts_the_equals_form() {
    let (_metrics_db_dir, metrics_db_path) = isolated_metrics_db_path();
    let repo =
        TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path.as_str())]);
    seed_ai_commit(&repo);

    let value = usage_json(&repo, &metrics_db_path, &["--period=3d"]);

    assert_window(&value, "last 3 days", 3);
}

#[test]
fn usage_without_period_defaults_to_thirty_day_window() {
    let (_metrics_db_dir, metrics_db_path) = isolated_metrics_db_path();
    let repo =
        TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path.as_str())]);
    seed_ai_commit(&repo);

    let value = usage_json(&repo, &metrics_db_path, &[]);

    assert_window(&value, "last 30 days", 30);
}

#[test]
fn usage_period_invalid_token_exits_nonzero_with_message() {
    let repo = TestRepo::new();

    let result = repo.git_ai(&["usage", "--period", "90d"]);

    let err = result.expect_err("an invalid --period value should exit non-zero");
    assert!(
        err.contains("Invalid --period value: 90d. Expected one of 1d, 3d, 7d, 30d."),
        "unexpected error output: {err}"
    );
}

#[test]
fn usage_period_missing_value_exits_nonzero_with_message() {
    let repo = TestRepo::new();

    let result = repo.git_ai(&["usage", "--period"]);

    let err = result.expect_err("a missing --period value should exit non-zero");
    assert!(
        err.contains("Missing value for --period."),
        "unexpected error output: {err}"
    );
}

#[test]
fn metrics_retention_days_prunes_existing_history() {
    // An empty override uses the file value. A valid override takes precedence.
    assert_seven_day_retention(7, "");
    assert_seven_day_retention(30, "7");
}

fn assert_seven_day_retention(file_days: u32, env_days: &str) {
    let (_metrics_dir, path) = isolated_metrics_db_path();
    let path_ref = std::path::Path::new(&path);
    git_ai::metrics::db::MetricsDatabase::open_at_path(path_ref).unwrap();
    let conn = git_ai::sqlite::open_with_memory_limits(path_ref).unwrap();
    for age in [8, 6] {
        let ts = (chrono::Utc::now() - chrono::Duration::days(age)).timestamp();
        let event = serde_json::json!({"t": ts, "e": 1, "v": {}, "a": {}});
        conn.execute(
            "INSERT INTO metrics (event_json, event_ts, delivered_ts) VALUES (?1, ?2, ?2)",
            rusqlite::params![event.to_string(), ts],
        )
        .unwrap();
    }

    let mut repo = TestRepo::new_with_daemon_scope(DaemonTestScope::NoDaemon);
    repo.patch_git_ai_config(|patch| patch.metrics_retention_days = Some(file_days));
    repo.start_dedicated_daemon_with_env_for_test(&[
        ("GIT_AI_TEST_METRICS_DB_PATH", &path),
        ("GIT_AI_METRICS_RETENTION_DAYS", env_days),
    ]);
    seed_ai_commit(&repo);
    repo.git_ai(&["await", "--timeout", "60"]).unwrap();

    let retained: Vec<i64> = conn
        .prepare("SELECT id FROM metrics WHERE id IN (1, 2) ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(retained, [2]);
}
