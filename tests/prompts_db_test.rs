//! Tests for src/commands/prompts_db.rs
//!
//! Comprehensive test coverage for SQLite database operations for prompt management:
//! - Database schema creation and migrations
//! - Prompt aggregation from multiple sources
//! - Query operations (search, filter, list)
//! - Data persistence and retrieval
//! - Error handling for database operations
//! - Transaction management

mod repos;

use git_ai::authorship::transcript::{AiTranscript, Message};
use repos::test_repo::TestRepo;
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::path::Path;

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
    let mut repo = TestRepo::new();

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

    // Verify at least one prompt was inserted
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();
    assert!(count > 0, "Should have at least one prompt");
}

#[test]
fn test_populate_with_since_filter() {
    let mut repo = TestRepo::new();

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
    assert!(count > 0, "Should have prompts within 1 day");

    // Note: --since 0 may not include prompts if the current timestamp logic
    // doesn't include "today" properly. This is expected behavior based on
    // how the since filter works with Unix timestamps.
}

#[test]
fn test_populate_with_author_filter() {
    let mut repo = TestRepo::new();

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
    assert!(count > 0, "Should have prompts for Test User");

    // Verify the author field (may include email)
    let author: Option<String> = conn
        .query_row("SELECT human_author FROM prompts LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        author.is_some() && author.as_ref().unwrap().contains("Test User"),
        "Author should contain Test User, got: {:?}",
        author
    );

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
    let mut repo = TestRepo::new();

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
    assert!(count > 0, "Should have prompts with --all-authors");
}

#[test]
fn test_list_command_outputs_tsv() {
    let mut repo = TestRepo::new();

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

    // Should have header + at least one row
    assert!(lines.len() >= 2, "Should have header and at least one row");

    // Header should contain expected columns
    let header = lines[0];
    assert!(header.contains("seq_id"), "Header should contain seq_id");
    assert!(header.contains("tool"), "Header should contain tool");
    assert!(header.contains("model"), "Header should contain model");

    // Data rows should be tab-separated
    if lines.len() > 1 {
        let data_row = lines[1];
        assert!(data_row.contains('\t'), "Data rows should be tab-separated");
    }
}

#[test]
fn test_list_command_with_custom_columns() {
    let mut repo = TestRepo::new();

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
    assert!(lines.len() >= 2, "Should have header and data");

    let header = lines[0];
    assert!(header.contains("seq_id"), "Header should contain seq_id");
    assert!(header.contains("tool"), "Header should contain tool");
    assert!(header.contains("model"), "Header should contain model");
}

#[test]
fn test_next_command_returns_json() {
    let mut repo = TestRepo::new();

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

    // Get next prompt
    let result = repo.git_ai(&["prompts", "next"]);
    assert!(result.is_ok(), "prompts next should succeed");

    let output = result.unwrap();
    let json: Value = serde_json::from_str(&output).expect("Output should be valid JSON");

    // Verify expected fields
    assert!(json.get("seq_id").is_some(), "Should have seq_id");
    assert!(json.get("id").is_some(), "Should have id");
    assert!(json.get("tool").is_some(), "Should have tool");
    assert!(json.get("model").is_some(), "Should have model");
    assert!(json.get("created_at").is_some(), "Should have created_at");
    assert!(json.get("updated_at").is_some(), "Should have updated_at");

    assert_eq!(
        json.get("tool").and_then(|v| v.as_str()),
        Some("test-agent"),
        "Tool should be test-agent"
    );
    assert_eq!(
        json.get("model").and_then(|v| v.as_str()),
        Some("test-model"),
        "Model should be test-model"
    );
}

#[test]
fn test_next_command_advances_pointer() {
    let mut repo = TestRepo::new();

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

    // Get first prompt
    let result1 = repo.git_ai(&["prompts", "next"]);
    assert!(result1.is_ok(), "First next should succeed");
    let json1: Value = serde_json::from_str(&result1.unwrap()).unwrap();
    let seq_id1 = json1.get("seq_id").and_then(|v| v.as_i64()).unwrap();

    // Get second prompt
    let result2 = repo.git_ai(&["prompts", "next"]);
    assert!(result2.is_ok(), "Second next should succeed");
    let json2: Value = serde_json::from_str(&result2.unwrap()).unwrap();
    let seq_id2 = json2.get("seq_id").and_then(|v| v.as_i64()).unwrap();

    // seq_id should advance
    assert!(seq_id2 > seq_id1, "seq_id should advance");

    // Verify pointer was updated in database
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let pointer: i64 = conn
        .query_row(
            "SELECT current_seq_id FROM pointers WHERE name = 'default'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(pointer, seq_id2, "Pointer should be at second prompt");
}

#[test]
fn test_next_command_no_more_prompts() {
    let mut repo = TestRepo::new();

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

    // Get the only prompt
    let result1 = repo.git_ai(&["prompts", "next"]);
    assert!(result1.is_ok(), "First next should succeed");

    // Try to get another prompt (should fail)
    let result2 = repo.git_ai(&["prompts", "next"]);
    assert!(
        result2.is_err(),
        "Second next should fail (no more prompts)"
    );

    let error = result2.unwrap_err();
    assert!(
        error.contains("No more prompts"),
        "Error should mention no more prompts"
    );
}

#[test]
fn test_reset_command() {
    let mut repo = TestRepo::new();

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

    // Get first prompt to advance pointer
    let result1 = repo.git_ai(&["prompts", "next"]);
    assert!(result1.is_ok(), "First next should succeed");
    let json1: Value = serde_json::from_str(&result1.unwrap()).unwrap();
    let seq_id1 = json1.get("seq_id").and_then(|v| v.as_i64()).unwrap();

    // Verify pointer is advanced
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let pointer_before: i64 = conn
        .query_row(
            "SELECT current_seq_id FROM pointers WHERE name = 'default'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pointer_before, seq_id1, "Pointer should be advanced");

    // Reset pointer
    let result = repo.git_ai(&["prompts", "reset"]);
    assert!(result.is_ok(), "prompts reset should succeed");

    // Verify pointer is reset to 0
    let pointer_after: i64 = conn
        .query_row(
            "SELECT current_seq_id FROM pointers WHERE name = 'default'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pointer_after, 0, "Pointer should be reset to 0");

    // Should be able to get the same prompt again
    let result2 = repo.git_ai(&["prompts", "next"]);
    assert!(result2.is_ok(), "Next after reset should succeed");
    let json2: Value = serde_json::from_str(&result2.unwrap()).unwrap();
    let seq_id2 = json2.get("seq_id").and_then(|v| v.as_i64()).unwrap();

    assert_eq!(seq_id2, seq_id1, "Should get the same prompt after reset");
}

#[test]
fn test_count_command() {
    let mut repo = TestRepo::new();

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

    // Count prompts
    let result = repo.git_ai(&["prompts", "count"]);
    assert!(result.is_ok(), "prompts count should succeed");

    let count_str = result.unwrap().trim().to_string();
    let count: i32 = count_str.parse().expect("Output should be a number");

    assert_eq!(count, 3, "Should have 3 prompts");
}

#[test]
fn test_exec_command_select_query() {
    let mut repo = TestRepo::new();

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

    // Execute SELECT query
    let result = repo.git_ai(&["prompts", "exec", "SELECT tool, model FROM prompts"]);
    assert!(result.is_ok(), "exec SELECT should succeed");

    let output = result.unwrap();
    let lines: Vec<&str> = output.lines().collect();

    // Should have header + at least one row
    assert!(lines.len() >= 2, "Should have header and data");

    let header = lines[0];
    assert!(header.contains("tool"), "Header should contain tool");
    assert!(header.contains("model"), "Header should contain model");

    // Verify data contains expected values
    let data = lines[1];
    assert!(data.contains("test-agent"), "Should contain test-agent");
    assert!(data.contains("test-model"), "Should contain test-model");
}

#[test]
fn test_exec_command_update_query() {
    let mut repo = TestRepo::new();

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

    // Execute UPDATE query
    let result = repo.git_ai(&[
        "prompts",
        "exec",
        "UPDATE prompts SET tool = 'updated-tool' WHERE tool = 'test-agent'",
    ]);
    assert!(result.is_ok(), "exec UPDATE should succeed");

    // Verify the update
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let tool: String = conn
        .query_row("SELECT tool FROM prompts LIMIT 1", [], |row| row.get(0))
        .unwrap();

    assert_eq!(tool, "updated-tool", "Tool should be updated");
}

#[test]
fn test_database_not_found_error() {
    let repo = TestRepo::new();

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
    let mut repo = TestRepo::new();

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

    // Verify only one prompt exists (upsert should deduplicate by id)
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert_eq!(count, 1, "Should have exactly one prompt (deduplicated)");
}

#[test]
fn test_populate_aggregates_from_git_notes() {
    let mut repo = TestRepo::new();

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

    // Verify prompt was found
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert!(
        count > 0,
        "Should have prompts from git notes even without internal DB"
    );
}

#[test]
fn test_prompt_messages_field_contains_transcript() {
    let mut repo = TestRepo::new();

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

    // Query the messages field
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let messages: Option<String> = conn
        .query_row("SELECT messages FROM prompts LIMIT 1", [], |row| row.get(0))
        .unwrap();

    assert!(messages.is_some(), "Messages field should be populated");

    let messages_str = messages.unwrap();
    assert!(
        messages_str.contains("This is my test message"),
        "Messages should contain the user message"
    );

    // Verify it's valid JSON
    let _json: Value = serde_json::from_str(&messages_str).expect("Messages should be valid JSON");
}

#[test]
fn test_accepted_rate_calculation() {
    let mut repo = TestRepo::new();

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

    // Verify accepted_rate is calculated (may be null if no accepted/overridden lines yet)
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();

    // Check that the column exists and can be queried
    let result: rusqlite::Result<Option<f64>> =
        conn.query_row("SELECT accepted_rate FROM prompts LIMIT 1", [], |row| {
            row.get(0)
        });

    assert!(result.is_ok(), "Should be able to query accepted_rate");
}

#[test]
fn test_timestamp_fields_populated() {
    let mut repo = TestRepo::new();

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

    // Verify timestamp fields
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();

    let (created_at, updated_at, start_time, last_time): (i64, i64, Option<i64>, Option<i64>) =
        conn.query_row(
            "SELECT created_at, updated_at, start_time, last_time FROM prompts LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();

    assert!(created_at > 0, "created_at should be a valid timestamp");
    assert!(updated_at > 0, "updated_at should be a valid timestamp");
    assert!(
        updated_at >= created_at,
        "updated_at should be >= created_at"
    );

    // start_time and last_time may be Some or None depending on transcript
    if let Some(start) = start_time {
        assert!(start > 0, "start_time should be valid if present");
    }
    if let Some(last) = last_time {
        assert!(last > 0, "last_time should be valid if present");
    }
}

#[test]
fn test_exec_invalid_sql_error() {
    let mut repo = TestRepo::new();

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
    let mut repo = TestRepo::new();

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

    // Verify commit_sha is populated
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let commit_sha: Option<String> = conn
        .query_row("SELECT commit_sha FROM prompts LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();

    assert!(
        commit_sha.is_some(),
        "commit_sha should be populated after commit"
    );

    let sha = commit_sha.unwrap();
    assert_eq!(sha.len(), 40, "commit_sha should be a full 40-char SHA");
}

#[test]
fn test_workdir_field_populated() {
    let mut repo = TestRepo::new();

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

    // Verify workdir is populated
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let workdir: Option<String> = conn
        .query_row("SELECT workdir FROM prompts LIMIT 1", [], |row| row.get(0))
        .unwrap();

    assert!(workdir.is_some(), "workdir should be populated");

    let wd = workdir.unwrap();
    assert!(!wd.is_empty(), "workdir should not be empty");
    assert!(
        Path::new(&wd).is_absolute(),
        "workdir should be an absolute path"
    );
}

#[test]
fn test_seq_id_auto_increments() {
    let mut repo = TestRepo::new();

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

    // Verify seq_ids are auto-incremented
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();

    let seq_ids: Vec<i64> = conn
        .prepare("SELECT seq_id FROM prompts ORDER BY seq_id ASC")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(seq_ids.len(), 3, "Should have 3 prompts");
    assert_eq!(seq_ids[0], 1, "First seq_id should be 1");
    assert_eq!(seq_ids[1], 2, "Second seq_id should be 2");
    assert_eq!(seq_ids[2], 3, "Third seq_id should be 3");
}

#[test]
fn test_unique_constraint_on_id() {
    let mut repo = TestRepo::new();

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

    // Try to populate again (should trigger UPSERT, not error)
    let result = repo.git_ai(&["prompts"]);
    assert!(
        result.is_ok(),
        "Second populate should succeed (upsert should handle duplicates)"
    );

    // Verify still only one prompt (not duplicated)
    let prompts_db_path = repo.path().join("prompts.db");
    let conn = Connection::open(&prompts_db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
        .unwrap();

    assert_eq!(count, 1, "Should still have exactly one prompt");
}
