use crate::repos::test_repo::TestRepo;

#[test]
fn test_checkpoint_debug_log_writes_when_enabled() {
    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.feature_flags = Some(serde_json::json!({"checkpoint_debug_log": true}));
    });

    let file_path = repo.path().join("test.txt");
    std::fs::write(&file_path, "hello\n").unwrap();

    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    let log_dir = repo
        .test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("checkpoint-debug-logs");
    assert!(log_dir.exists(), "checkpoint-debug-logs dir should exist");

    let entries: Vec<_> = std::fs::read_dir(&log_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "should have exactly one daily log file");

    let content = std::fs::read_to_string(entries[0].path()).unwrap();
    let line: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
    assert_eq!(line["preset_name"], "mock_known_human");
    assert!(line["trace_id"].is_string());
    assert!(line["timestamp"].is_string());
    assert!(line["event_count"].is_number());
    assert!(line["requests"].is_array());
}

#[test]
fn test_checkpoint_debug_log_does_not_write_when_disabled() {
    let repo = TestRepo::new();

    let file_path = repo.path().join("test.txt");
    std::fs::write(&file_path, "hello\n").unwrap();

    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    let log_dir = repo
        .test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("checkpoint-debug-logs");
    assert!(
        !log_dir.exists(),
        "checkpoint-debug-logs dir should NOT exist when flag is off"
    );
}

#[test]
fn test_checkpoint_debug_log_appends_multiple_entries() {
    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.feature_flags = Some(serde_json::json!({"checkpoint_debug_log": true}));
    });

    let file_path = repo.path().join("test.txt");
    std::fs::write(&file_path, "hello\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    std::fs::write(&file_path, "hello\nworld\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();

    let log_dir = repo
        .test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("checkpoint-debug-logs");

    let entries: Vec<_> = std::fs::read_dir(&log_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1);

    let content = std::fs::read_to_string(entries[0].path()).unwrap();
    let lines: Vec<&str> = content.trim().lines().collect();
    assert_eq!(lines.len(), 2, "should have two JSONL entries");

    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(first["preset_name"], "mock_known_human");
    assert_eq!(second["preset_name"], "mock_ai");
}
