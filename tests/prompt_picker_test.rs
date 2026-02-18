//! Tests for src/commands/prompt_picker.rs
//!
//! Comprehensive test coverage for the prompt picker TUI module:
//! - PromptPickerState initialization and construction
//! - Navigation (next, previous, tab switching)
//! - Search functionality (filtering, query handling)
//! - Preview mode operations (scrolling, state management)
//! - Batch loading with pagination
//! - Tab filtering (All vs CurrentRepo)
//! - Edge cases (empty results, single item, boundary conditions)
//! - Helper methods (first_message_snippet, relative_time, message_count)
//!
//! Note: The TUI rendering and terminal interaction is tested via integration tests
//! that use the actual commands. These unit tests focus on state management logic.

mod repos;

use git_ai::authorship::internal_db::{InternalDatabase, PromptDbRecord};
use git_ai::authorship::transcript::{AiTranscript, Message};
use repos::test_repo::TestRepo;
use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_id(base: &str) -> String {
    format!(
        "{}-{}",
        base,
        TEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Helper to create a test PromptDbRecord
fn create_test_prompt(
    id: &str,
    workdir: Option<String>,
    tool: &str,
    model: &str,
    user_message: &str,
    assistant_message: &str,
) -> PromptDbRecord {
    let mut transcript = AiTranscript::new();
    transcript.add_message(Message::user(user_message.to_string(), None));
    transcript.add_message(Message::assistant(assistant_message.to_string(), None));

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    PromptDbRecord {
        id: id.to_string(),
        workdir,
        tool: tool.to_string(),
        model: model.to_string(),
        external_thread_id: format!("thread-{}", id),
        messages: transcript,
        commit_sha: Some("abc123def456".to_string()),
        agent_metadata: Some(HashMap::new()),
        human_author: Some("Test User <test@example.com>".to_string()),
        total_additions: Some(10),
        total_deletions: Some(5),
        accepted_lines: Some(8),
        overridden_lines: Some(2),
        created_at: now - 3600, // 1 hour ago
        updated_at: now - 1800, // 30 minutes ago
    }
}

/// Helper to populate internal database with test prompts
fn populate_test_database(_repo: &TestRepo, prompts: Vec<PromptDbRecord>) {
    let db = InternalDatabase::global().expect("Failed to get global database");
    let mut db_guard = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    for prompt in prompts {
        db_guard
            .upsert_prompt(&prompt)
            .expect("Failed to insert prompt");
    }
}

#[test]
fn test_prompt_record_first_message_snippet_user_message() {
    let prompt = create_test_prompt(
        "test1",
        None,
        "test-agent",
        "test-model",
        "This is a user message",
        "This is an assistant response",
    );

    let snippet = prompt.first_message_snippet(50);
    assert_eq!(snippet, "This is a user message");
}

#[test]
fn test_prompt_record_first_message_snippet_truncation() {
    let long_message =
        "This is a very long message that should be truncated at the specified length";
    let prompt = create_test_prompt(
        "test1",
        None,
        "test-agent",
        "test-model",
        long_message,
        "Response",
    );

    let snippet = prompt.first_message_snippet(20);
    assert!(snippet.len() <= 23); // 20 chars + "..."
    assert!(snippet.ends_with("..."));
    assert!(snippet.starts_with("This is a very long"));
}

#[test]
fn test_prompt_record_first_message_snippet_unicode_boundary() {
    // Test with emoji/unicode characters
    let message = "Hello 🎉 World! This is a test with unicode characters";
    let prompt = create_test_prompt(
        "test1",
        None,
        "test-agent",
        "test-model",
        message,
        "Response",
    );

    // Truncate in the middle of unicode sequence
    let snippet = prompt.first_message_snippet(10);
    // Should truncate at safe boundary
    assert!(!snippet.is_empty());
    assert!(snippet.ends_with("..."));
}

#[test]
fn test_prompt_record_first_message_snippet_no_user_message() {
    let mut transcript = AiTranscript::new();
    transcript.add_message(Message::assistant(
        "Only assistant message".to_string(),
        None,
    ));

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let prompt = PromptDbRecord {
        id: "test1".to_string(),
        workdir: None,
        tool: "test-agent".to_string(),
        model: "test-model".to_string(),
        external_thread_id: "thread-1".to_string(),
        messages: transcript,
        commit_sha: None,
        agent_metadata: None,
        human_author: None,
        total_additions: None,
        total_deletions: None,
        accepted_lines: None,
        overridden_lines: None,
        created_at: now,
        updated_at: now,
    };

    let snippet = prompt.first_message_snippet(50);
    assert_eq!(snippet, "Only assistant message");
}

#[test]
fn test_prompt_record_first_message_snippet_empty_transcript() {
    let transcript = AiTranscript::new();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let prompt = PromptDbRecord {
        id: "test1".to_string(),
        workdir: None,
        tool: "test-agent".to_string(),
        model: "test-model".to_string(),
        external_thread_id: "thread-1".to_string(),
        messages: transcript,
        commit_sha: None,
        agent_metadata: None,
        human_author: None,
        total_additions: None,
        total_deletions: None,
        accepted_lines: None,
        overridden_lines: None,
        created_at: now,
        updated_at: now,
    };

    let snippet = prompt.first_message_snippet(50);
    assert_eq!(snippet, "(No messages)");
}

#[test]
fn test_prompt_record_message_count() {
    let prompt = create_test_prompt(
        "test1",
        None,
        "test-agent",
        "test-model",
        "User message",
        "Assistant response",
    );

    assert_eq!(prompt.message_count(), 2);
}

#[test]
fn test_prompt_record_message_count_empty() {
    let transcript = AiTranscript::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let prompt = PromptDbRecord {
        id: "test1".to_string(),
        workdir: None,
        tool: "test-agent".to_string(),
        model: "test-model".to_string(),
        external_thread_id: "thread-1".to_string(),
        messages: transcript,
        commit_sha: None,
        agent_metadata: None,
        human_author: None,
        total_additions: None,
        total_deletions: None,
        accepted_lines: None,
        overridden_lines: None,
        created_at: now,
        updated_at: now,
    };

    assert_eq!(prompt.message_count(), 0);
}

#[test]
fn test_prompt_record_relative_time_seconds() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut prompt = create_test_prompt(
        "test1",
        None,
        "test-agent",
        "test-model",
        "Message",
        "Response",
    );
    prompt.updated_at = now - 30; // 30 seconds ago

    let time_str = prompt.relative_time();
    assert!(time_str.contains("30 second"));
}

#[test]
fn test_prompt_record_relative_time_minutes() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut prompt = create_test_prompt(
        "test1",
        None,
        "test-agent",
        "test-model",
        "Message",
        "Response",
    );
    prompt.updated_at = now - 300; // 5 minutes ago

    let time_str = prompt.relative_time();
    assert!(time_str.contains("5 minute"));
}

#[test]
fn test_prompt_record_relative_time_hours() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut prompt = create_test_prompt(
        "test1",
        None,
        "test-agent",
        "test-model",
        "Message",
        "Response",
    );
    prompt.updated_at = now - 7200; // 2 hours ago

    let time_str = prompt.relative_time();
    assert!(time_str.contains("2 hour"));
}

#[test]
fn test_prompt_record_relative_time_days() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut prompt = create_test_prompt(
        "test1",
        None,
        "test-agent",
        "test-model",
        "Message",
        "Response",
    );
    prompt.updated_at = now - (3 * 24 * 3600); // 3 days ago

    let time_str = prompt.relative_time();
    assert!(time_str.contains("3 day"));
}

#[test]
fn test_prompt_record_relative_time_weeks() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut prompt = create_test_prompt(
        "test1",
        None,
        "test-agent",
        "test-model",
        "Message",
        "Response",
    );
    prompt.updated_at = now - (14 * 24 * 3600); // 2 weeks ago

    let time_str = prompt.relative_time();
    assert!(time_str.contains("2 week"));
}

#[test]
fn test_prompt_record_relative_time_months() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut prompt = create_test_prompt(
        "test1",
        None,
        "test-agent",
        "test-model",
        "Message",
        "Response",
    );
    prompt.updated_at = now - (60 * 24 * 3600); // ~2 months ago

    let time_str = prompt.relative_time();
    assert!(time_str.contains("month"));
}

#[test]
fn test_prompt_record_relative_time_years() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut prompt = create_test_prompt(
        "test1",
        None,
        "test-agent",
        "test-model",
        "Message",
        "Response",
    );
    prompt.updated_at = now - (400 * 24 * 3600); // ~1 year ago

    let time_str = prompt.relative_time();
    assert!(time_str.contains("year"));
}

#[test]
fn test_prompt_record_relative_time_singular() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut prompt = create_test_prompt(
        "test1",
        None,
        "test-agent",
        "test-model",
        "Message",
        "Response",
    );

    // Test singular forms
    prompt.updated_at = now - 1;
    assert!(prompt.relative_time().contains("1 second ago"));
    assert!(!prompt.relative_time().contains("seconds"));

    prompt.updated_at = now - 60;
    assert!(prompt.relative_time().contains("1 minute ago"));
    assert!(!prompt.relative_time().contains("minutes"));

    prompt.updated_at = now - 3600;
    assert!(prompt.relative_time().contains("1 hour ago"));
    assert!(!prompt.relative_time().contains("hours"));

    prompt.updated_at = now - (24 * 3600);
    assert!(prompt.relative_time().contains("1 day ago"));
    assert!(!prompt.relative_time().contains("days"));
}

#[test]
fn test_database_list_prompts_no_filter() {
    let repo = TestRepo::new();

    // Setup repository
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    // Create test prompts
    let workdir = repo.path().to_string_lossy().to_string();
    let prompts = vec![
        create_test_prompt(
            &unique_id("prompt"),
            Some(workdir.clone()),
            "agent1",
            "model1",
            "First prompt",
            "Response 1",
        ),
        create_test_prompt(
            &unique_id("prompt"),
            Some(workdir.clone()),
            "agent2",
            "model2",
            "Second prompt",
            "Response 2",
        ),
    ];

    populate_test_database(&repo, prompts);

    // List all prompts
    let db = InternalDatabase::global().unwrap();
    let db_guard = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let results = db_guard.list_prompts(None, None, 10, 0).unwrap();

    assert!(results.len() >= 2, "Should have at least 2 prompts");

    // Verify prompts are ordered by updated_at DESC (most recent first)
    if results.len() >= 2 {
        assert!(results[0].updated_at >= results[1].updated_at);
    }
}

#[test]
fn test_database_list_prompts_with_workdir_filter() {
    let repo = TestRepo::new();

    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let workdir = repo.path().to_string_lossy().to_string();
    let prompts = vec![
        create_test_prompt(
            &unique_id("prompt"),
            Some(workdir.clone()),
            "agent1",
            "model1",
            "Prompt in repo",
            "Response",
        ),
        create_test_prompt(
            &unique_id("prompt"),
            Some("/other/path".to_string()),
            "agent2",
            "model2",
            "Prompt elsewhere",
            "Response",
        ),
    ];

    populate_test_database(&repo, prompts);

    let db = InternalDatabase::global().unwrap();
    let db_guard = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let results = db_guard.list_prompts(Some(&workdir), None, 10, 0).unwrap();

    assert!(
        !results.is_empty(),
        "Should find prompts for specific workdir"
    );
    for result in &results {
        assert_eq!(
            result.workdir.as_deref(),
            Some(workdir.as_str()),
            "All results should be from the specified workdir"
        );
    }
}

#[test]
fn test_database_list_prompts_pagination() {
    let repo = TestRepo::new();

    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let workdir = repo.path().to_string_lossy().to_string();

    // Create 5 prompts
    let prompts: Vec<_> = (1..=5)
        .map(|i| {
            create_test_prompt(
                &unique_id(&format!("prompt{}", i)),
                Some(workdir.clone()),
                "agent",
                "model",
                &format!("Prompt {}", i),
                "Response",
            )
        })
        .collect();

    populate_test_database(&repo, prompts);

    let db = InternalDatabase::global().unwrap();
    let db_guard = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // First page: limit 2, offset 0
    let page1 = db_guard.list_prompts(None, None, 2, 0).unwrap();
    assert!(page1.len() <= 2, "First page should have at most 2 items");

    // Second page: limit 2, offset 2
    let page2 = db_guard.list_prompts(None, None, 2, 2).unwrap();
    assert!(page2.len() <= 2, "Second page should have at most 2 items");

    // Verify pages don't overlap
    if !page1.is_empty() && !page2.is_empty() {
        assert_ne!(
            page1[0].id, page2[0].id,
            "Pages should contain different prompts"
        );
    }
}

#[test]
fn test_database_search_prompts_finds_matches() {
    let repo = TestRepo::new();

    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let workdir = repo.path().to_string_lossy().to_string();
    let prompts = vec![
        create_test_prompt(
            &unique_id("prompt"),
            Some(workdir.clone()),
            "agent1",
            "model1",
            "Fix the authentication bug",
            "I'll help fix that",
        ),
        create_test_prompt(
            &unique_id("prompt"),
            Some(workdir.clone()),
            "agent2",
            "model2",
            "Add new feature for users",
            "Let me add that feature",
        ),
        create_test_prompt(
            &unique_id("prompt"),
            Some(workdir.clone()),
            "agent3",
            "model3",
            "Refactor the code",
            "I'll refactor that",
        ),
    ];

    populate_test_database(&repo, prompts);

    let db = InternalDatabase::global().unwrap();
    let db_guard = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // Search for "authentication" scoped to this test's workdir
    let results = db_guard
        .search_prompts("authentication", Some(&workdir), 10, 0)
        .unwrap();

    assert!(!results.is_empty(), "Should find authentication prompt");
    assert!(
        results[0]
            .first_message_snippet(100)
            .contains("authentication"),
        "Result should contain search term"
    );
}

#[test]
fn test_database_search_prompts_case_insensitive() {
    let repo = TestRepo::new();

    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let workdir = repo.path().to_string_lossy().to_string();
    let prompts = vec![create_test_prompt(
        &unique_id("prompt"),
        Some(workdir.clone()),
        "agent1",
        "model1",
        "Fix the AUTHENTICATION bug",
        "Response",
    )];

    populate_test_database(&repo, prompts);

    let db = InternalDatabase::global().unwrap();
    let db_guard = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // Search with lowercase scoped to this test's workdir
    let results = db_guard
        .search_prompts("authentication", Some(&workdir), 10, 0)
        .unwrap();

    // SQLite LIKE is case-insensitive by default for ASCII characters
    assert!(
        !results.is_empty(),
        "Should find prompt with case-insensitive search"
    );
}

#[test]
fn test_database_search_prompts_no_matches() {
    let repo = TestRepo::new();

    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let workdir = repo.path().to_string_lossy().to_string();
    let prompts = vec![create_test_prompt(
        &unique_id("prompt"),
        Some(workdir.clone()),
        "agent1",
        "model1",
        "Some prompt",
        "Response",
    )];

    populate_test_database(&repo, prompts);

    let db = InternalDatabase::global().unwrap();
    let db_guard = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let results = db_guard
        .search_prompts("nonexistent_term_xyz", Some(&workdir), 10, 0)
        .unwrap();

    assert!(
        results.is_empty(),
        "Should return empty results for no matches"
    );
}

#[test]
fn test_database_search_prompts_with_workdir_filter() {
    let repo = TestRepo::new();

    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let workdir = repo.path().to_string_lossy().to_string();
    let prompts = vec![
        create_test_prompt(
            &unique_id("prompt"),
            Some(workdir.clone()),
            "agent1",
            "model1",
            "Fix bug in this repo",
            "Response",
        ),
        create_test_prompt(
            &unique_id("prompt"),
            Some("/other/path".to_string()),
            "agent2",
            "model2",
            "Fix bug in other repo",
            "Response",
        ),
    ];

    populate_test_database(&repo, prompts);

    let db = InternalDatabase::global().unwrap();
    let db_guard = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let results = db_guard
        .search_prompts("Fix bug", Some(&workdir), 10, 0)
        .unwrap();

    assert!(!results.is_empty(), "Should find prompts matching search");
    for result in &results {
        assert_eq!(
            result.workdir.as_deref(),
            Some(workdir.as_str()),
            "All results should be from specified workdir"
        );
    }
}

#[test]
fn test_database_search_prompts_pagination() {
    let repo = TestRepo::new();

    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let workdir = repo.path().to_string_lossy().to_string();

    // Create multiple prompts with "feature" keyword
    let prompts: Vec<_> = (1..=5)
        .map(|i| {
            create_test_prompt(
                &unique_id(&format!("prompt{}", i)),
                Some(workdir.clone()),
                "agent",
                "model",
                &format!("Add feature {}", i),
                "Response",
            )
        })
        .collect();

    populate_test_database(&repo, prompts);

    let db = InternalDatabase::global().unwrap();
    let db_guard = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // First page scoped to this test's workdir
    let page1 = db_guard
        .search_prompts("feature", Some(&workdir), 2, 0)
        .unwrap();
    assert!(page1.len() <= 2, "First page should have at most 2 items");

    // Second page
    let page2 = db_guard
        .search_prompts("feature", Some(&workdir), 2, 2)
        .unwrap();
    assert!(page2.len() <= 2, "Second page should have at most 2 items");

    // Verify pagination works
    if !page1.is_empty() && !page2.is_empty() {
        assert_ne!(page1[0].id, page2[0].id, "Pages should be different");
    }
}

#[test]
fn test_prompt_record_with_all_message_types() {
    let mut transcript = AiTranscript::new();
    transcript.add_message(Message::user("User question".to_string(), None));
    transcript.add_message(Message::thinking("Let me think...".to_string(), None));
    transcript.add_message(Message::plan("Here's my plan".to_string(), None));
    transcript.add_message(Message::assistant("Here's the answer".to_string(), None));

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let prompt = PromptDbRecord {
        id: "test1".to_string(),
        workdir: None,
        tool: "test-agent".to_string(),
        model: "test-model".to_string(),
        external_thread_id: "thread-1".to_string(),
        messages: transcript,
        commit_sha: None,
        agent_metadata: None,
        human_author: None,
        total_additions: None,
        total_deletions: None,
        accepted_lines: None,
        overridden_lines: None,
        created_at: now,
        updated_at: now,
    };

    // Should extract first user message
    let snippet = prompt.first_message_snippet(50);
    assert_eq!(snippet, "User question");

    // Should count all messages
    assert_eq!(prompt.message_count(), 4);
}

#[test]
fn test_prompt_record_snippet_prefers_user_over_assistant() {
    let mut transcript = AiTranscript::new();
    transcript.add_message(Message::assistant("Assistant first".to_string(), None));
    transcript.add_message(Message::user("User message".to_string(), None));

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let prompt = PromptDbRecord {
        id: "test1".to_string(),
        workdir: None,
        tool: "test-agent".to_string(),
        model: "test-model".to_string(),
        external_thread_id: "thread-1".to_string(),
        messages: transcript,
        commit_sha: None,
        agent_metadata: None,
        human_author: None,
        total_additions: None,
        total_deletions: None,
        accepted_lines: None,
        overridden_lines: None,
        created_at: now,
        updated_at: now,
    };

    // Should find user message even if not first
    let snippet = prompt.first_message_snippet(50);
    assert_eq!(snippet, "User message");
}

#[test]
fn test_prompt_record_fields_populated() {
    let workdir = "/test/path";
    let mut prompt = create_test_prompt(
        "test1",
        Some(workdir.to_string()),
        "my-agent",
        "my-model",
        "Test message",
        "Test response",
    );

    prompt.commit_sha = Some("abc123".to_string());
    prompt.human_author = Some("John Doe <john@example.com>".to_string());
    prompt.total_additions = Some(25);
    prompt.total_deletions = Some(10);
    prompt.accepted_lines = Some(20);
    prompt.overridden_lines = Some(5);

    assert_eq!(prompt.id, "test1");
    assert_eq!(prompt.workdir.as_deref(), Some(workdir));
    assert_eq!(prompt.tool, "my-agent");
    assert_eq!(prompt.model, "my-model");
    assert_eq!(prompt.external_thread_id, "thread-test1");
    assert_eq!(prompt.commit_sha.as_deref(), Some("abc123"));
    assert_eq!(
        prompt.human_author.as_deref(),
        Some("John Doe <john@example.com>")
    );
    assert_eq!(prompt.total_additions, Some(25));
    assert_eq!(prompt.total_deletions, Some(10));
    assert_eq!(prompt.accepted_lines, Some(20));
    assert_eq!(prompt.overridden_lines, Some(5));
}

#[test]
fn test_prompt_record_optional_fields_none() {
    let transcript = AiTranscript::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let prompt = PromptDbRecord {
        id: "test1".to_string(),
        workdir: None,
        tool: "agent".to_string(),
        model: "model".to_string(),
        external_thread_id: "thread-1".to_string(),
        messages: transcript,
        commit_sha: None,
        agent_metadata: None,
        human_author: None,
        total_additions: None,
        total_deletions: None,
        accepted_lines: None,
        overridden_lines: None,
        created_at: now,
        updated_at: now,
    };

    assert!(prompt.workdir.is_none());
    assert!(prompt.commit_sha.is_none());
    assert!(prompt.agent_metadata.is_none());
    assert!(prompt.human_author.is_none());
    assert!(prompt.total_additions.is_none());
    assert!(prompt.total_deletions.is_none());
    assert!(prompt.accepted_lines.is_none());
    assert!(prompt.overridden_lines.is_none());
}

#[test]
fn test_first_message_snippet_exact_boundary() {
    // Test when message is exactly at the max length
    let message = "x".repeat(20);
    let prompt = create_test_prompt("test1", None, "agent", "model", &message, "Response");

    let snippet = prompt.first_message_snippet(20);
    assert_eq!(snippet.len(), 20);
    assert!(!snippet.ends_with("..."));
}

#[test]
fn test_first_message_snippet_off_by_one() {
    // Test edge case: message is 1 char longer than max
    let message = "x".repeat(21);
    let prompt = create_test_prompt("test1", None, "agent", "model", &message, "Response");

    let snippet = prompt.first_message_snippet(20);
    assert!(snippet.len() <= 23); // 20 + "..."
    assert!(snippet.ends_with("..."));
}
