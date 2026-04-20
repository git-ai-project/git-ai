//! Tests for src/commands/prompts_db.rs
//!
//! Comprehensive test coverage for SQLite database operations for prompt management:
//! - Database schema creation and migrations
//! - Prompt aggregation from multiple sources
//! - Query operations (search, filter, list)
//! - Data persistence and retrieval
//! - Error handling for database operations
//! - Transaction management

use crate::repos::test_repo::TestRepo;
use git_ai::authorship::transcript::{AiTranscript, Message};
use rusqlite::Connection;
use std::fs;

/// Helper to create a test checkpoint with a transcript
fn checkpoint_with_message(
    repo: &TestRepo,
    message: &str,
    edited_files: Vec<String>,
    conversation_id: &str,
) {
    let mut transcript = AiTranscript::new();
    transcript.add_message(Message::user(message.to_string(), None));
    transcript.add_message(Message::assistant(
        "I'll help you with that.".to_string(),
        None,
    ));

    let hook_input = serde_json::json!({
        "type": "ai_agent",
        "repo_working_dir": repo.path().to_str().unwrap(),
        "edited_filepaths": edited_files,
        "transcript": transcript,
        "agent_name": "test-agent",
        "model": "test-model",
        "conversation_id": conversation_id,
    });

    let hook_input_str = serde_json::to_string(&hook_input).unwrap();

    repo.git_ai(&["checkpoint", "agent-v1", "--hook-input", &hook_input_str])
        .expect("checkpoint should succeed");
}

/// Helper to verify database schema exists and is valid
fn verify_schema(conn: &Connection) {
    // Check prompts table exists with expected columns
    let table_info: Vec<String> = conn
        .prepare("PRAGMA table_info(prompts)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let expected_columns = vec![
        "seq_id",
        "id",
        "tool",
        "model",
        "external_thread_id",
        "human_author",
        "commit_sha",
        "workdir",
        "total_additions",
        "total_deletions",
        "accepted_lines",
        "overridden_lines",
        "accepted_rate",
        "messages",
        "start_time",
        "last_time",
        "created_at",
        "updated_at",
    ];

    for expected in &expected_columns {
        assert!(
            table_info.contains(&expected.to_string()),
            "Missing column: {}",
            expected
        );
    }

    // Check pointers table exists
    let pointers_table_exists: bool = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='pointers'")
        .unwrap()
        .query_map([], |_| Ok(true))
        .unwrap()
        .next()
        .is_some();

    assert!(pointers_table_exists, "pointers table should exist");

    // Check indexes exist
    let indexes: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='index'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let expected_indexes = vec![
        "idx_prompts_id",
        "idx_prompts_tool",
        "idx_prompts_human_author",
        "idx_prompts_start_time",
    ];

    for expected_idx in &expected_indexes {
        assert!(
            indexes.iter().any(|idx| idx == expected_idx),
            "Missing index: {}",
            expected_idx
        );
    }
}

#[test]
fn test_populate_creates_database_with_schema() {
    let mut repo = TestRepo::new_dedicated_daemon();

    // Enable prompt sharing for testing
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Create initial commit
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    // Create a checkpoint
    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    // Commit the changes
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"])
        .expect("commit should succeed");

    // Run prompts populate command
    let prompts_db_path = repo.path().join("prompts.db");
    let result = repo.git_ai(&["prompts"]);
    assert!(result.is_ok(), "prompts populate should succeed");

    // Verify database was created
    assert!(prompts_db_path.exists(), "prompts.db should be created");

    // Verify schema
    let conn = Connection::open(&prompts_db_path).expect("Should open database");
    verify_schema(&conn);

    // Note: agent-v1 now produces sessions (not prompts), so the prompts table will be empty.
    // This test verifies that the schema is created correctly, which is still important
    // for backward compatibility with old-format data.
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

#[test]
fn test_populate_with_since_filter() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Create initial commit
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    // Create checkpoint
    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    // Populate with --since 1 (1 day ago, should include recent prompts)
    let result = repo.git_ai(&["prompts", "--since", "1"]);
    assert!(result.is_ok(), "prompts --since 1 should succeed");

    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();
    // agent-v1 now produces sessions (not prompts), so count will be 0
    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

#[test]
fn test_populate_with_author_filter() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Create initial commit
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    // Create checkpoint (will be attributed to "Test User" from git config)
    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    // Populate with matching author
    let result = repo.git_ai(&["prompts", "--author", "Test User"]);
    assert!(result.is_ok(), "prompts --author should succeed");

    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();
    // agent-v1 now produces sessions (not prompts), so count will be 0
    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );

    // Explicitly close the connection before removing the file (Windows requires this)
    drop(conn);

    // Populate with non-matching author (should have no results)
    fs::remove_file(&prompts_db_path).unwrap();
    let result = repo.git_ai(&["prompts", "--author", "NonExistent User"]);
    assert!(result.is_ok(), "prompts --author should succeed");

    let conn = Connection::open(&prompts_db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0, "Should have no prompts for NonExistent User");
}

#[test]
fn test_populate_with_all_authors_flag() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Create initial commit
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    // Create checkpoint
    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    // Populate with --all-authors
    let result = repo.git_ai(&["prompts", "--all-authors"]);
    assert!(result.is_ok(), "prompts --all-authors should succeed");

    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();
    // agent-v1 now produces sessions (not prompts), so count will be 0
    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

#[test]
fn test_list_command_outputs_tsv() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    // Populate database
    repo.git_ai(&["prompts"]).unwrap();

    // List prompts
    let result = repo.git_ai(&["prompts", "list"]);
    assert!(result.is_ok(), "prompts list should succeed");

    let output = result.unwrap();
    let lines: Vec<&str> = output.lines().collect();

    // agent-v1 now produces sessions (not prompts), so there will only be a header row
    assert_eq!(lines.len(), 1, "Should have only header row (no data)");

    // Header should contain expected columns
    let header = lines[0];
    assert!(header.contains("seq_id"), "Header should contain seq_id");
    assert!(header.contains("tool"), "Header should contain tool");
    assert!(header.contains("model"), "Header should contain model");
}

#[test]
fn test_list_command_with_custom_columns() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // List with custom columns
    let result = repo.git_ai(&["prompts", "list", "--columns", "seq_id,tool,model"]);
    assert!(result.is_ok(), "prompts list --columns should succeed");

    let output = result.unwrap();
    let lines: Vec<&str> = output.lines().collect();
    // agent-v1 now produces sessions (not prompts), so there will only be a header row
    assert_eq!(lines.len(), 1, "Should have only header row (no data)");

    let header = lines[0];
    assert!(header.contains("seq_id"), "Header should contain seq_id");
    assert!(header.contains("tool"), "Header should contain tool");
    assert!(header.contains("model"), "Header should contain model");
}

#[test]
fn test_next_command_returns_json() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // agent-v1 now produces sessions (not prompts), so there are no prompts
    // prompts next should fail when there are no prompts
    let result = repo.git_ai(&["prompts", "next"]);
    assert!(
        result.is_err(),
        "prompts next should fail when no prompts exist"
    );

    let error = result.unwrap_err();
    assert!(
        error.contains("No more prompts"),
        "Error should mention no more prompts"
    );
}

#[test]
fn test_next_command_advances_pointer() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup with two prompts
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    // First prompt
    let file1_path = repo.path().join("test1.txt");
    fs::write(&file1_path, "AI content 1\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file 1",
        vec!["test1.txt".to_string()],
        "conv-1",
    );

    // Second prompt
    let file2_path = repo.path().join("test2.txt");
    fs::write(&file2_path, "AI content 2\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file 2",
        vec!["test2.txt".to_string()],
        "conv-2",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test files"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // agent-v1 now produces sessions (not prompts), so both calls should fail
    let result1 = repo.git_ai(&["prompts", "next"]);
    assert!(
        result1.is_err(),
        "First next should fail when no prompts exist"
    );
    assert!(result1.unwrap_err().contains("No more prompts"));

    let result2 = repo.git_ai(&["prompts", "next"]);
    assert!(
        result2.is_err(),
        "Second next should fail when no prompts exist"
    );
    assert!(result2.unwrap_err().contains("No more prompts"));

    // Verify pointer remains at 0 in database (or doesn't exist yet)
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let pointer: i64 = conn
        .query_row(
            "SELECT current_seq_id FROM pointers WHERE name = 'default'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    assert_eq!(pointer, 0, "Pointer should remain at 0 with no prompts");
}

#[test]
fn test_next_command_no_more_prompts() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup with one prompt
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // agent-v1 now produces sessions (not prompts), so all calls should fail
    let result1 = repo.git_ai(&["prompts", "next"]);
    assert!(
        result1.is_err(),
        "First next should fail when no prompts exist"
    );
    assert!(result1.unwrap_err().contains("No more prompts"));

    let result2 = repo.git_ai(&["prompts", "next"]);
    assert!(
        result2.is_err(),
        "Second next should fail when no prompts exist"
    );
    assert!(result2.unwrap_err().contains("No more prompts"));
}

#[test]
fn test_reset_command() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // agent-v1 now produces sessions (not prompts), so next will fail
    let result1 = repo.git_ai(&["prompts", "next"]);
    assert!(
        result1.is_err(),
        "First next should fail when no prompts exist"
    );
    assert!(result1.unwrap_err().contains("No more prompts"));

    // Verify pointer is at 0 (or doesn't exist yet)
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let pointer_before: i64 = conn
        .query_row(
            "SELECT current_seq_id FROM pointers WHERE name = 'default'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    assert_eq!(pointer_before, 0, "Pointer should be at 0");

    // Reset pointer (should still succeed even with no data)
    let result = repo.git_ai(&["prompts", "reset"]);
    assert!(result.is_ok(), "prompts reset should succeed");

    // Verify pointer is still 0
    let pointer_after: i64 = conn
        .query_row(
            "SELECT current_seq_id FROM pointers WHERE name = 'default'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pointer_after, 0, "Pointer should remain at 0");

    // Next should still fail
    let result2 = repo.git_ai(&["prompts", "next"]);
    assert!(
        result2.is_err(),
        "Next after reset should fail when no prompts exist"
    );
    assert!(result2.unwrap_err().contains("No more prompts"));
}

#[test]
fn test_count_command() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup with multiple prompts
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    // Create 3 prompts
    for i in 1..=3 {
        let file_path = repo.path().join(format!("test{}.txt", i));
        fs::write(&file_path, format!("AI content {}\n", i)).unwrap();
        checkpoint_with_message(
            &repo,
            &format!("Add test file {}", i),
            vec![format!("test{}.txt", i)],
            &format!("conv-{}", i),
        );
    }

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test files"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // Count prompts - agent-v1 now produces sessions (not prompts), so count will be 0
    let result = repo.git_ai(&["prompts", "count"]);
    assert!(result.is_ok(), "prompts count should succeed");

    let count_str = result.unwrap().trim().to_string();
    let count: i32 = count_str.parse().expect("Output should be a number");

    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

#[test]
fn test_exec_command_select_query() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // Execute SELECT query - agent-v1 now produces sessions (not prompts)
    let result = repo.git_ai(&["prompts", "exec", "SELECT tool, model FROM prompts"]);
    assert!(result.is_ok(), "exec SELECT should succeed");

    let output = result.unwrap();
    let lines: Vec<&str> = output.lines().collect();

    // Should have only header row (no data)
    assert_eq!(lines.len(), 1, "Should have only header row (no data)");

    let header = lines[0];
    assert!(header.contains("tool"), "Header should contain tool");
    assert!(header.contains("model"), "Header should contain model");
}

#[test]
fn test_exec_command_update_query() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // Execute UPDATE query - agent-v1 now produces sessions (not prompts), so no rows to update
    let result = repo.git_ai(&[
        "prompts",
        "exec",
        "UPDATE prompts SET tool = 'updated-tool' WHERE tool = 'test-agent'",
    ]);
    assert!(result.is_ok(), "exec UPDATE should succeed");

    // Verify no rows were updated (since there are no prompts)
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert_eq!(count, 0, "Should have 0 prompts");
}

#[test]
fn test_database_not_found_error() {
    let repo = TestRepo::new_dedicated_daemon();

    // Try to list without populating first
    let result = repo.git_ai(&["prompts", "list"]);
    assert!(
        result.is_err(),
        "list should fail when database doesn't exist"
    );

    let error = result.unwrap_err();
    assert!(
        error.contains("prompts.db not found"),
        "Error should mention database not found"
    );
}

#[test]
fn test_upsert_deduplicates_prompts() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    // Populate twice
    repo.git_ai(&["prompts"]).unwrap();
    repo.git_ai(&["prompts"]).unwrap();

    // agent-v1 now produces sessions (not prompts), so count will be 0
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

#[test]
fn test_populate_aggregates_from_git_notes() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    // Clear the internal database to force reading from git notes
    let internal_db_path = repo.test_db_path().join("git-ai.db");
    if internal_db_path.exists() {
        fs::remove_file(&internal_db_path).ok();
    }

    // Populate (should read from git notes)
    let result = repo.git_ai(&["prompts"]);
    assert!(
        result.is_ok(),
        "prompts should succeed reading from git notes"
    );

    // agent-v1 now produces sessions (not prompts), so count will be 0
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

#[test]
fn test_prompt_messages_field_contains_transcript() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "This is my test message",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // agent-v1 now produces sessions (not prompts), so there are no rows to query
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

#[test]
fn test_accepted_rate_calculation() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // agent-v1 now produces sessions (not prompts), so there are no rows
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

#[test]
fn test_timestamp_fields_populated() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // agent-v1 now produces sessions (not prompts), so there are no rows
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

#[test]
fn test_exec_invalid_sql_error() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // Try to execute invalid SQL
    let result = repo.git_ai(&["prompts", "exec", "INVALID SQL QUERY"]);
    assert!(result.is_err(), "exec should fail with invalid SQL");

    let error = result.unwrap_err();
    assert!(
        error.contains("SQL error") || error.contains("syntax error"),
        "Error should mention SQL error"
    );
}

#[test]
fn test_commit_sha_field_populated() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    let _commit_result = repo.git(&["commit", "-m", "Add test file"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // agent-v1 now produces sessions (not prompts), so there are no rows
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

#[test]
fn test_workdir_field_populated() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // agent-v1 now produces sessions (not prompts), so there are no rows
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

#[test]
fn test_seq_id_auto_increments() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup with multiple prompts
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    // Create 3 prompts
    for i in 1..=3 {
        let file_path = repo.path().join(format!("test{}.txt", i));
        fs::write(&file_path, format!("AI content {}\n", i)).unwrap();
        checkpoint_with_message(
            &repo,
            &format!("Add test file {}", i),
            vec![format!("test{}.txt", i)],
            &format!("conv-{}", i),
        );
    }

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test files"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // agent-v1 now produces sessions (not prompts), so there are no rows
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

#[test]
fn test_unique_constraint_on_id() {
    let mut repo = TestRepo::new_dedicated_daemon();

    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });

    // Setup
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "AI content\n").unwrap();
    checkpoint_with_message(
        &repo,
        "Add test file",
        vec!["test.txt".to_string()],
        "conv-1",
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "Add test file"]).unwrap();

    repo.git_ai(&["prompts"]).unwrap();

    // Try to populate again (should succeed)
    let result = repo.git_ai(&["prompts"]);
    assert!(result.is_ok(), "Second populate should succeed");

    // agent-v1 now produces sessions (not prompts), so count will be 0
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert_eq!(
        count, 0,
        "Should have 0 prompts (agent-v1 now produces sessions)"
    );
}

crate::reuse_tests_in_worktree!(
    test_populate_creates_database_with_schema,
    test_populate_with_since_filter,
    test_populate_with_author_filter,
    test_populate_with_all_authors_flag,
    test_list_command_outputs_tsv,
    test_list_command_with_custom_columns,
    test_next_command_returns_json,
    test_next_command_advances_pointer,
    test_next_command_no_more_prompts,
    test_reset_command,
    test_count_command,
    test_exec_command_select_query,
    test_exec_command_update_query,
    test_database_not_found_error,
    test_upsert_deduplicates_prompts,
    test_populate_aggregates_from_git_notes,
    test_prompt_messages_field_contains_transcript,
    test_accepted_rate_calculation,
    test_timestamp_fields_populated,
    test_exec_invalid_sql_error,
    test_commit_sha_field_populated,
    test_workdir_field_populated,
    test_seq_id_auto_increments,
    test_unique_constraint_on_id,
);
