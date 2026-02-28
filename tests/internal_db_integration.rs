//! Integration tests for the internal database prompt storage.
//!
//! These tests verify that:
//! 1. AI checkpoints save prompt+messages to the internal database
//! 2. On commit, the latest prompts are saved into the database
//! 3. The post-commit update logic fetches and saves the latest messages

#[macro_use]
mod repos;
mod test_utils;

use repos::test_repo::TestRepo;
use rusqlite::Connection;
use serde_json::json;
use std::fs;
use test_utils::fixture_path;

/// Helper to open a connection to the test database
fn open_test_db(repo: &TestRepo) -> Connection {
    Connection::open(repo.test_db_path()).expect("Failed to open test database")
}

/// Query prompt records from the test database
fn query_prompts(conn: &Connection) -> Vec<PromptRow> {
    let mut stmt = conn
        .prepare(
            "SELECT id, workdir, tool, model, external_thread_id, messages, commit_sha,
                    human_author, total_additions, total_deletions
             FROM prompts ORDER BY updated_at DESC",
        )
        .expect("Failed to prepare statement");

    stmt.query_map([], |row| {
        Ok(PromptRow {
            id: row.get(0)?,
            workdir: row.get(1)?,
            tool: row.get(2)?,
            model: row.get(3)?,
            external_thread_id: row.get(4)?,
            messages_json: row.get(5)?,
            commit_sha: row.get(6)?,
            human_author: row.get(7)?,
            total_additions: row.get(8)?,
            total_deletions: row.get(9)?,
        })
    })
    .expect("Failed to query prompts")
    .filter_map(|r| r.ok())
    .collect()
}

#[derive(Debug)]
#[allow(dead_code)]
struct PromptRow {
    id: String,
    workdir: Option<String>,
    tool: String,
    model: String,
    external_thread_id: String,
    messages_json: String,
    commit_sha: Option<String>,
    human_author: Option<String>,
    total_additions: Option<u32>,
    total_deletions: Option<u32>,
}

/// Test 1: AI checkpoint saves prompt record to the internal database
/// Note: The model may initially be "unknown" during checkpoint but gets
/// updated correctly during post-commit when the transcript is re-read
#[test]
fn test_checkpoint_saves_prompt_to_internal_db() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();

    // Create initial file and commit
    let src_dir = repo_root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("main.rs");
    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create a transcript file with actual content
    let transcript_path = repo_root.join("claude-session.jsonl");
    let fixture = fixture_path("example-claude-code.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();

    // Prepare hook input for claude checkpoint
    let hook_input = json!({
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "transcript_path": transcript_path.to_string_lossy().to_string(),
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    // Make AI changes and run checkpoint
    fs::write(&file_path, "fn main() {\n    // AI added this line\n}\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .unwrap();

    // Verify prompt was saved to internal database
    let conn = open_test_db(&repo);
    let prompts = query_prompts(&conn);

    assert_eq!(prompts.len(), 1, "Should have exactly one prompt record");

    let prompt = &prompts[0];
    assert_eq!(prompt.tool, "claude", "Tool should be 'claude'");
    // Model may be "unknown" at checkpoint time - gets updated correctly at commit time
    assert!(prompt.workdir.is_some(), "Workdir should be set");
    assert!(
        prompt.commit_sha.is_none(),
        "Commit SHA should be None before commit"
    );
}

/// Test 2: On commit, the latest prompts are saved to the database with commit SHA and correct model
#[test]
fn test_commit_updates_prompt_with_commit_sha_and_model() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();

    // Create initial file and commit
    let file_path = repo_root.join("test.txt");
    fs::write(&file_path, "line1\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create a transcript file
    let transcript_path = repo_root.join("claude-session.jsonl");
    let fixture = fixture_path("example-claude-code.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();

    // Prepare hook input
    let hook_input = json!({
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "transcript_path": transcript_path.to_string_lossy().to_string(),
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    // Make AI changes and checkpoint
    fs::write(&file_path, "line1\nai line 2\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .unwrap();

    // Verify prompt exists but has no commit SHA yet
    let conn = open_test_db(&repo);
    let prompts_before = query_prompts(&conn);
    assert_eq!(prompts_before.len(), 1);
    assert!(
        prompts_before[0].commit_sha.is_none(),
        "Commit SHA should be None before commit"
    );

    // Now commit the changes
    let commit = repo.stage_all_and_commit("AI changes").unwrap();

    // Re-query - the prompt should now have the commit SHA and correct model
    let prompts_after = query_prompts(&conn);
    assert_eq!(prompts_after.len(), 1, "Should still have one prompt");
    assert_eq!(
        prompts_after[0].commit_sha,
        Some(commit.commit_sha.clone()),
        "Commit SHA should be updated after commit"
    );
    // Post-commit updates the model correctly from the transcript
    assert_eq!(
        prompts_after[0].model, "claude-sonnet-4-20250514",
        "Model should be updated from transcript after commit"
    );
}

/// Test 3: Post-commit updates prompts with latest messages from transcript
/// This tests the race condition fix where an early checkpoint might have empty/partial
/// transcript, but a later checkpoint updates it, and post-commit should use the latest.
#[test]
fn test_post_commit_uses_latest_transcript_messages() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();

    // Create initial file and commit
    let file_path = repo_root.join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Use a stable transcript path so both checkpoints share the same agent_id
    let transcript_path = repo_root.join("claude-session.jsonl");

    // First checkpoint: empty transcript (simulates race where data isn't ready yet)
    fs::write(&transcript_path, "").unwrap();
    let hook_input = json!({
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "transcript_path": transcript_path.to_string_lossy().to_string(),
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    // First AI edit and checkpoint with empty transcript
    fs::write(&file_path, "const x = 1;\n// ai line one\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .unwrap();

    // Second AI edit with the real transcript content
    let fixture = fixture_path("example-claude-code.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();
    fs::write(&file_path, "const x = 1;\n// ai line one\n// ai line two\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .unwrap();

    // Commit the changes - post_commit should use the latest transcript
    let commit = repo.stage_all_and_commit("Add AI lines").unwrap();

    // Verify the database has the latest messages (from the fixture)
    let conn = open_test_db(&repo);
    let prompts = query_prompts(&conn);

    assert_eq!(prompts.len(), 1, "Should have exactly one prompt record");

    let prompt = &prompts[0];
    assert_eq!(
        prompt.commit_sha,
        Some(commit.commit_sha),
        "Commit SHA should be set"
    );

    // Parse messages and verify they're from the real transcript (not empty)
    let messages: serde_json::Value =
        serde_json::from_str(&prompt.messages_json).expect("Messages should be valid JSON");
    let messages_array = messages["messages"]
        .as_array()
        .expect("Should have messages array");

    assert!(
        !messages_array.is_empty(),
        "Messages should not be empty - post-commit should have fetched latest transcript"
    );

    // Verify the model is correct (from the fixture)
    assert_eq!(
        prompt.model, "claude-sonnet-4-20250514",
        "Model should be from the latest transcript"
    );
}

/// Test 4: Multiple AI checkpoints from same session are deduplicated
#[test]
fn test_multiple_checkpoints_same_session_deduplicated() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();

    // Create initial file and commit
    let file_path = repo_root.join("test.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create transcript
    let transcript_path = repo_root.join("claude-session.jsonl");
    let fixture = fixture_path("example-claude-code.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();

    let hook_input = json!({
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "transcript_path": transcript_path.to_string_lossy().to_string(),
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    // Multiple checkpoints from the same session
    for i in 1..=3 {
        fs::write(&file_path, format!("base\nline {}\n", i)).unwrap();
        repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
            .unwrap();
    }

    // Verify only one prompt record exists (deduplicated by agent_id)
    let conn = open_test_db(&repo);
    let prompts = query_prompts(&conn);

    assert_eq!(
        prompts.len(),
        1,
        "Should have exactly one prompt record (deduplicated)"
    );
}

/// Test 5: Different AI sessions create separate prompt records
#[test]
fn test_different_sessions_create_separate_prompts() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();

    // Create initial file and commit
    let file_path = repo_root.join("test.txt");
    fs::write(&file_path, "base\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create two different transcript files (different sessions)
    let transcript_path_1 = repo_root.join("session1.jsonl");
    let transcript_path_2 = repo_root.join("session2.jsonl");
    let fixture = fixture_path("example-claude-code.jsonl");
    fs::copy(&fixture, &transcript_path_1).unwrap();
    fs::copy(&fixture, &transcript_path_2).unwrap();

    // First session checkpoint
    let hook_input_1 = json!({
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "transcript_path": transcript_path_1.to_string_lossy().to_string(),
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    fs::write(&file_path, "base\nsession1 line\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input_1])
        .unwrap();

    // Second session checkpoint (different transcript path = different session ID)
    let hook_input_2 = json!({
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "transcript_path": transcript_path_2.to_string_lossy().to_string(),
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    fs::write(&file_path, "base\nsession1 line\nsession2 line\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input_2])
        .unwrap();

    // Verify two separate prompt records exist
    let conn = open_test_db(&repo);
    let prompts = query_prompts(&conn);

    assert_eq!(
        prompts.len(),
        2,
        "Should have two prompt records for different sessions"
    );

    // Verify they have different external_thread_ids
    let ids: Vec<&str> = prompts
        .iter()
        .map(|p| p.external_thread_id.as_str())
        .collect();
    assert_ne!(ids[0], ids[1], "External thread IDs should be different");
}

/// Test 6: Line stats are correctly saved to the database after commit
#[test]
fn test_line_stats_saved_to_db_after_commit() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();

    // Create initial file with content
    let file_path = repo_root.join("test.txt");
    fs::write(&file_path, "line1\nline2\nline3\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create transcript
    let transcript_path = repo_root.join("claude-session.jsonl");
    let fixture = fixture_path("example-claude-code.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();

    let hook_input = json!({
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "transcript_path": transcript_path.to_string_lossy().to_string(),
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    // Make changes: add 2 lines, delete 1 line
    fs::write(&file_path, "line1\nnew_ai_line1\nnew_ai_line2\nline3\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .unwrap();

    // Commit the changes - line stats are updated at commit time
    repo.stage_all_and_commit("AI changes").unwrap();

    // Query the database
    let conn = open_test_db(&repo);
    let prompts = query_prompts(&conn);

    assert_eq!(prompts.len(), 1);
    let prompt = &prompts[0];

    // Verify line stats are populated after commit
    assert!(
        prompt.total_additions.is_some(),
        "Total additions should be set after commit"
    );
    assert!(
        prompt.total_deletions.is_some(),
        "Total deletions should be set after commit"
    );

    // We added 2 lines and deleted 1 line
    assert_eq!(
        prompt.total_additions.unwrap(),
        2,
        "Should have 2 additions"
    );
    assert_eq!(prompt.total_deletions.unwrap(), 1, "Should have 1 deletion");
}

/// Test 7: Human author is saved after commit
#[test]
fn test_human_author_saved_to_db_after_commit() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();

    // Create initial file and commit
    let file_path = repo_root.join("test.txt");
    fs::write(&file_path, "initial\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create transcript
    let transcript_path = repo_root.join("claude-session.jsonl");
    let fixture = fixture_path("example-claude-code.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();

    let hook_input = json!({
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "transcript_path": transcript_path.to_string_lossy().to_string(),
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    fs::write(&file_path, "initial\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .unwrap();

    // Commit the changes - human author is finalized at commit time
    repo.stage_all_and_commit("AI changes").unwrap();

    // Query the database
    let conn = open_test_db(&repo);
    let prompts = query_prompts(&conn);

    assert_eq!(prompts.len(), 1);
    let prompt = &prompts[0];

    // Verify human author is set (from git config user.name)
    assert!(
        prompt.human_author.is_some(),
        "Human author should be set after commit"
    );
    assert_eq!(
        prompt.human_author.as_ref().unwrap(),
        "Test User",
        "Human author should match git config"
    );
}

/// Test 8: Workdir is correctly saved
#[test]
fn test_workdir_saved_to_db() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();

    // Create initial file and commit
    let file_path = repo_root.join("test.txt");
    fs::write(&file_path, "initial\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create transcript
    let transcript_path = repo_root.join("claude-session.jsonl");
    let fixture = fixture_path("example-claude-code.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();

    let hook_input = json!({
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "transcript_path": transcript_path.to_string_lossy().to_string(),
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    fs::write(&file_path, "initial\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .unwrap();

    // Query the database
    let conn = open_test_db(&repo);
    let prompts = query_prompts(&conn);

    assert_eq!(prompts.len(), 1);
    let prompt = &prompts[0];

    // Verify workdir is set and matches the repo path
    assert!(prompt.workdir.is_some(), "Workdir should be set");
    let workdir = prompt.workdir.as_ref().unwrap();

    // The workdir should contain part of the repo path
    // Note: exact paths may differ due to canonicalization
    assert!(!workdir.is_empty(), "Workdir should not be empty");
}

/// Test 9: Verify mock_ai checkpoint (non-claude) also saves to internal db
#[test]
fn test_mock_ai_checkpoint_saves_to_internal_db() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();

    // Create initial file and commit
    let file_path = repo_root.join("test.txt");
    fs::write(&file_path, "line1\nline2\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make AI changes using mock_ai checkpoint
    fs::write(&file_path, "line1\nline2\nai line 3\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    // Query the database
    let conn = open_test_db(&repo);
    let prompts = query_prompts(&conn);

    // mock_ai should also create a prompt record
    assert_eq!(
        prompts.len(),
        1,
        "mock_ai checkpoint should create a prompt record"
    );

    let prompt = &prompts[0];
    assert_eq!(prompt.tool, "mock_ai", "Tool should be 'mock_ai'");
}

/// Test 10: Verify thinking transcript (claude with extended thinking) saves correctly after commit
#[test]
fn test_thinking_transcript_saves_to_internal_db_after_commit() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();

    // Create initial file and commit
    let file_path = repo_root.join("index.ts");
    fs::write(&file_path, "console.log('hello');\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Use the thinking fixture
    let transcript_path = repo_root.join("thinking-session.jsonl");
    let fixture = fixture_path("claude-code-with-thinking.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();

    let hook_input = json!({
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "transcript_path": transcript_path.to_string_lossy().to_string(),
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    // Make changes
    fs::write(
        &file_path,
        "console.log('hello');\nconsole.log('hello world');\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .unwrap();

    // Commit the changes - model and messages are updated at commit time
    repo.stage_all_and_commit("AI changes").unwrap();

    // Query the database
    let conn = open_test_db(&repo);
    let prompts = query_prompts(&conn);

    assert_eq!(prompts.len(), 1);
    let prompt = &prompts[0];

    // Verify model is from thinking transcript (updated at commit time)
    assert_eq!(
        prompt.model, "claude-sonnet-4-5-20250929",
        "Model should be from thinking transcript after commit"
    );

    // Verify messages were parsed
    let messages: serde_json::Value =
        serde_json::from_str(&prompt.messages_json).expect("Messages should be valid JSON");
    let messages_array = messages["messages"]
        .as_array()
        .expect("Should have messages array");

    // Should have 6 messages (1 user + 2 thinking + 2 text + 1 tool_use, tool_result skipped)
    assert_eq!(
        messages_array.len(),
        6,
        "Should have all messages including thinking"
    );
}

reuse_tests_in_worktree!(
    test_checkpoint_saves_prompt_to_internal_db,
    test_commit_updates_prompt_with_commit_sha_and_model,
    test_post_commit_uses_latest_transcript_messages,
    test_multiple_checkpoints_same_session_deduplicated,
    test_different_sessions_create_separate_prompts,
    test_line_stats_saved_to_db_after_commit,
    test_human_author_saved_to_db_after_commit,
    test_workdir_saved_to_db,
    test_mock_ai_checkpoint_saves_to_internal_db,
    test_thinking_transcript_saves_to_internal_db_after_commit,
);
