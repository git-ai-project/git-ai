use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log::PromptRecord;
use git_ai::authorship::prompt_utils::{
    PromptUpdateResult, find_prompt, find_prompt_in_commit, find_prompt_in_history,
    find_prompt_with_db_fallback, format_transcript, update_claude_prompt, update_codex_prompt,
    update_continue_cli_prompt, update_droid_prompt, update_gemini_prompt,
    update_github_copilot_prompt, update_prompt_from_tool, update_windsurf_prompt,
};
use git_ai::authorship::transcript::Message;
use git_ai::authorship::working_log::AgentId;
use git_ai::git::refs::get_authorship;
use git_ai::git::repository::find_repository_in_path;
use std::collections::HashMap;

// Helper function to create a test PromptRecord
fn create_test_prompt_record(tool: &str, id: &str, model: &str) -> PromptRecord {
    PromptRecord {
        agent_id: AgentId {
            tool: tool.to_string(),
            id: id.to_string(),
            model: model.to_string(),
        },
        human_author: Some("test_user".to_string()),
        messages: vec![
            Message::User {
                text: "Hello".to_string(),
                timestamp: None,
            },
            Message::Assistant {
                text: "Hi there".to_string(),
                timestamp: None,
            },
        ],
        total_additions: 10,
        total_deletions: 5,
        accepted_lines: 8,
        overriden_lines: 2,
        messages_url: None,
        custom_attributes: None,
    }
}

#[test]
fn test_format_transcript_basic() {
    let prompt = create_test_prompt_record("test", "123", "gpt-4");
    let formatted = format_transcript(&prompt);

    assert!(formatted.contains("User: Hello\n"));
    assert!(formatted.contains("Assistant: Hi there\n"));
}

#[test]
fn test_format_transcript_all_message_types() {
    let mut prompt = create_test_prompt_record("test", "123", "gpt-4");
    prompt.messages = vec![
        Message::User {
            text: "User message".to_string(),
            timestamp: None,
        },
        Message::Assistant {
            text: "Assistant message".to_string(),
            timestamp: None,
        },
        Message::Thinking {
            text: "Thinking message".to_string(),
            timestamp: None,
        },
        Message::Plan {
            text: "Plan message".to_string(),
            timestamp: None,
        },
        Message::ToolUse {
            name: "test_tool".to_string(),
            input: serde_json::json!({"param": "value"}),
            timestamp: None,
        },
    ];

    let formatted = format_transcript(&prompt);

    assert!(formatted.contains("User: User message\n"));
    assert!(formatted.contains("Assistant: Assistant message\n"));
    assert!(formatted.contains("Thinking: Thinking message\n"));
    assert!(formatted.contains("Plan: Plan message\n"));
    // ToolUse should be filtered out
    assert!(!formatted.contains("test_tool"));
    assert!(!formatted.contains("ToolUse"));
}

#[test]
fn test_format_transcript_empty() {
    let mut prompt = create_test_prompt_record("test", "123", "gpt-4");
    prompt.messages = vec![];

    let formatted = format_transcript(&prompt);
    assert_eq!(formatted, "");
}

#[test]
fn test_format_transcript_multiline() {
    let mut prompt = create_test_prompt_record("test", "123", "gpt-4");
    prompt.messages = vec![Message::User {
        text: "Line 1\nLine 2\nLine 3".to_string(),
        timestamp: None,
    }];

    let formatted = format_transcript(&prompt);
    assert_eq!(formatted, "User: Line 1\nLine 2\nLine 3\n");
}

#[test]
fn test_update_prompt_from_tool_unknown() {
    let result = update_prompt_from_tool("unknown-tool", "thread-123", None, "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_codex_prompt_no_metadata() {
    let result = update_codex_prompt(None, "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_codex_prompt_no_transcript_path() {
    let metadata = HashMap::new();
    let result = update_codex_prompt(Some(&metadata), "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_claude_prompt_no_metadata() {
    let result = update_claude_prompt(None, "claude-3");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_claude_prompt_no_transcript_path() {
    let metadata = HashMap::new();
    let result = update_claude_prompt(Some(&metadata), "claude-3");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_gemini_prompt_no_metadata() {
    let result = update_gemini_prompt(None, "gemini-pro");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_gemini_prompt_no_transcript_path() {
    let metadata = HashMap::new();
    let result = update_gemini_prompt(Some(&metadata), "gemini-pro");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_github_copilot_prompt_no_metadata() {
    let result = update_github_copilot_prompt(None, "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_github_copilot_prompt_no_session_path() {
    let metadata = HashMap::new();
    let result = update_github_copilot_prompt(Some(&metadata), "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_continue_cli_prompt_no_metadata() {
    let result = update_continue_cli_prompt(None, "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_continue_cli_prompt_no_transcript_path() {
    let metadata = HashMap::new();
    let result = update_continue_cli_prompt(Some(&metadata), "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_droid_prompt_no_metadata() {
    let result = update_droid_prompt(None, "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_droid_prompt_no_transcript_path() {
    let metadata = HashMap::new();
    let result = update_droid_prompt(Some(&metadata), "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

// mock_ai preset does not produce transcript/prompt records — these tests
// need a preset that generates prompts (e.g. codex with a transcript fixture).
#[test]
#[ignore]
fn test_find_prompt_in_commit_integration() {
    // Create a test repository
    let repo = TestRepo::new();

    // Create initial commit with AI checkpoint
    std::fs::write(repo.path().join("test.txt"), "initial content\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Get authorship log from the commit
    let authorship = get_authorship(&gitai_repo, &head_sha).unwrap();
    let prompt_id = authorship.metadata.prompts.keys().next().unwrap().clone();

    // Test finding the prompt
    let result = find_prompt_in_commit(&gitai_repo, &prompt_id, "HEAD");
    assert!(result.is_ok());

    let (commit_sha, prompt) = result.unwrap();
    assert_eq!(commit_sha, head_sha);
    assert_eq!(prompt.agent_id.tool, "mock_ai");
    // Note: agent_id.id is auto-generated by mock_ai, not fixed
    assert!(prompt.agent_id.id.starts_with("ai-thread-"));
    assert_eq!(prompt.agent_id.model, "unknown");
}

#[test]
fn test_find_prompt_in_commit_not_found() {
    let repo = TestRepo::new();

    // Create commit with a known-human checkpoint so authorship data exists but has no prompts
    std::fs::write(repo.path().join("test.txt"), "initial content\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Try to find a non-existent prompt — authorship exists but prompt does not
    let result = find_prompt_in_commit(&gitai_repo, "nonexistent-prompt", "HEAD");
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Prompt") && err_msg.contains("not found"),
        "Unexpected error: {}",
        err_msg
    );
}

#[test]
fn test_find_prompt_in_commit_invalid_revision() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "initial content\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let result = find_prompt_in_commit(&gitai_repo, "any-prompt", "invalid-revision");
    assert!(result.is_err());
}

#[test]
#[ignore]
fn test_find_prompt_in_history_basic() {
    let repo = TestRepo::new();

    // Create first commit with AI checkpoint
    std::fs::write(repo.path().join("test.txt"), "v1\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.stage_all_and_commit("First commit").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Get authorship log from the commit
    let authorship = get_authorship(&gitai_repo, &head_sha).unwrap();
    let prompt_id = authorship
        .metadata
        .prompts
        .keys()
        .next()
        .expect("No prompt found")
        .clone();

    // Test finding the prompt with offset 0 (most recent)
    let result = find_prompt_in_history(&gitai_repo, &prompt_id, 0);
    assert!(result.is_ok());

    let (_sha, prompt) = result.unwrap();
    assert_eq!(prompt.agent_id.tool, "mock_ai");
    // Note: agent_id.id is auto-generated by mock_ai, not fixed
    assert!(prompt.agent_id.id.starts_with("ai-thread-"));
}

#[test]
#[ignore]
fn test_find_prompt_in_history_with_offset() {
    let repo = TestRepo::new();

    // Create first commit
    std::fs::write(repo.path().join("test.txt"), "v1\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Commit 1").unwrap();

    // Get prompt ID from first commit
    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let authorship = get_authorship(&gitai_repo, &head_sha).unwrap();
    let prompt_id = authorship
        .metadata
        .prompts
        .keys()
        .next()
        .expect("No prompt found")
        .clone();

    // At this point, offset 0 should work, offset 1 should fail
    let result = find_prompt_in_history(&gitai_repo, &prompt_id, 0);
    assert!(result.is_ok());

    let result = find_prompt_in_history(&gitai_repo, &prompt_id, 1);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("found 1 time(s), but offset 1 requested")
    );
}

#[test]
fn test_find_prompt_in_history_not_found() {
    let repo = TestRepo::new();

    // Create commit with known-human checkpoint so authorship data exists but no prompts
    std::fs::write(repo.path().join("test.txt"), "content\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();
    repo.stage_all_and_commit("Commit").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let result = find_prompt_in_history(&gitai_repo, "nonexistent-prompt", 0);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Prompt not found in history")
    );
}

#[test]
#[ignore]
fn test_find_prompt_delegates_to_commit() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "content\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Test commit").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let authorship = get_authorship(&gitai_repo, &head_sha).unwrap();
    let prompt_id = authorship
        .metadata
        .prompts
        .keys()
        .next()
        .expect("No prompt found")
        .clone();

    // Test with commit specified
    let result = find_prompt(&gitai_repo, &prompt_id, Some("HEAD"), 0);
    assert!(result.is_ok());
    let (_sha, prompt) = result.unwrap();
    assert_eq!(prompt.agent_id.tool, "mock_ai");
    // Note: agent_id.id is auto-generated by mock_ai, not fixed
    assert!(prompt.agent_id.id.starts_with("ai-thread-"));
}

#[test]
#[ignore]
fn test_find_prompt_delegates_to_history() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "content\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Test commit").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let authorship = get_authorship(&gitai_repo, &head_sha).unwrap();
    let prompt_id = authorship
        .metadata
        .prompts
        .keys()
        .next()
        .expect("No prompt found")
        .clone();

    // Test without commit (searches history)
    let result = find_prompt(&gitai_repo, &prompt_id, None, 0);
    assert!(result.is_ok());
    let (_sha, prompt) = result.unwrap();
    assert_eq!(prompt.agent_id.tool, "mock_ai");
    // Note: agent_id.id is auto-generated by mock_ai, not fixed
    assert!(prompt.agent_id.id.starts_with("ai-thread-"));
}

#[test]
fn test_find_prompt_with_db_fallback_no_db_no_repo() {
    // Test when prompt is not in DB and no repo is provided
    let result = find_prompt_with_db_fallback("nonexistent-prompt", None);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("not found in database and no repository provided")
    );
}

#[test]
#[ignore]
fn test_find_prompt_with_db_fallback_no_db_with_repo() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "content\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Test commit").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let authorship = get_authorship(&gitai_repo, &head_sha).unwrap();
    let prompt_id = authorship
        .metadata
        .prompts
        .keys()
        .next()
        .expect("No prompt found")
        .clone();

    // Test fallback to repository
    let result = find_prompt_with_db_fallback(&prompt_id, Some(&gitai_repo));
    assert!(result.is_ok());
    let (commit_sha, prompt) = result.unwrap();
    assert!(commit_sha.is_some());
    assert_eq!(prompt.agent_id.tool, "mock_ai");
}

#[test]
fn test_find_prompt_with_db_fallback_not_in_repo() {
    let repo = TestRepo::new();

    // Create commit with known-human checkpoint so authorship data exists but no prompts
    std::fs::write(repo.path().join("test.txt"), "content\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();
    repo.stage_all_and_commit("Test commit").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let result = find_prompt_with_db_fallback("nonexistent-prompt", Some(&gitai_repo));
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("not found in database or repository")
    );
}

#[test]
fn test_update_prompt_from_tool_dispatch() {
    // Test that unknown tools return Unchanged
    let result = update_prompt_from_tool("unknown", "thread-123", None, "model");
    assert!(matches!(result, PromptUpdateResult::Unchanged));

    // Test dispatch to cursor (may return Failed if cursor DB doesn't exist, which is expected)
    let result = update_prompt_from_tool("cursor", "thread-123", None, "model");
    assert!(matches!(
        result,
        PromptUpdateResult::Unchanged | PromptUpdateResult::Failed(_)
    ));

    // Test dispatch to claude
    let result = update_prompt_from_tool("claude", "thread-123", None, "model");
    assert!(matches!(result, PromptUpdateResult::Unchanged));

    // Test dispatch to codex
    let result = update_prompt_from_tool("codex", "thread-123", None, "model");
    assert!(matches!(result, PromptUpdateResult::Unchanged));

    // Test dispatch to gemini
    let result = update_prompt_from_tool("gemini", "thread-123", None, "model");
    assert!(matches!(result, PromptUpdateResult::Unchanged));

    // Test dispatch to github-copilot
    let result = update_prompt_from_tool("github-copilot", "thread-123", None, "model");
    assert!(matches!(result, PromptUpdateResult::Unchanged));

    // Test dispatch to continue-cli
    let result = update_prompt_from_tool("continue-cli", "thread-123", None, "model");
    assert!(matches!(result, PromptUpdateResult::Unchanged));

    // Test dispatch to droid
    let result = update_prompt_from_tool("droid", "thread-123", None, "model");
    assert!(matches!(result, PromptUpdateResult::Unchanged));

    // Test dispatch to amp (without metadata, returns Unchanged or Failed depending on local state)
    let result = update_prompt_from_tool("amp", "thread-123", None, "model");
    assert!(matches!(
        result,
        PromptUpdateResult::Unchanged | PromptUpdateResult::Failed(_)
    ));

    // Test dispatch to opencode (behavior depends on whether default storage exists)
    let result = update_prompt_from_tool("opencode", "session-123", None, "model");
    // Can be Unchanged, Failed, or Updated depending on storage availability
    match result {
        PromptUpdateResult::Unchanged
        | PromptUpdateResult::Failed(_)
        | PromptUpdateResult::Updated(_, _) => {}
    }

    // Test dispatch to windsurf
    let result = update_prompt_from_tool("windsurf", "trajectory-123", None, "model");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_format_transcript_with_timestamps() {
    let mut prompt = create_test_prompt_record("test", "123", "gpt-4");
    prompt.messages = vec![
        Message::User {
            text: "Question".to_string(),
            timestamp: Some("2024-01-01T12:00:00Z".to_string()),
        },
        Message::Assistant {
            text: "Answer".to_string(),
            timestamp: Some("2024-01-01T12:00:01Z".to_string()),
        },
    ];

    let formatted = format_transcript(&prompt);
    // Timestamps should not appear in formatted output
    assert!(!formatted.contains("2024-01-01"));
    assert!(formatted.contains("User: Question\n"));
    assert!(formatted.contains("Assistant: Answer\n"));
}

#[test]
fn test_format_transcript_special_characters() {
    let mut prompt = create_test_prompt_record("test", "123", "gpt-4");
    prompt.messages = vec![Message::User {
        text: "Text with \"quotes\" and 'apostrophes' and\ttabs\nand newlines".to_string(),
        timestamp: None,
    }];

    let formatted = format_transcript(&prompt);
    assert!(formatted.contains("\"quotes\""));
    assert!(formatted.contains("'apostrophes'"));
    assert!(formatted.contains("\t"));
}

#[test]
fn test_format_transcript_unicode() {
    let mut prompt = create_test_prompt_record("test", "123", "gpt-4");
    prompt.messages = vec![Message::User {
        text: "Hello 世界 🌍 Здравствуй مرحبا".to_string(),
        timestamp: None,
    }];

    let formatted = format_transcript(&prompt);
    assert!(formatted.contains("世界"));
    assert!(formatted.contains("🌍"));
    assert!(formatted.contains("Здравствуй"));
    assert!(formatted.contains("مرحبا"));
}

#[test]
fn test_update_codex_prompt_invalid_path() {
    let mut metadata = HashMap::new();
    metadata.insert(
        "transcript_path".to_string(),
        "/nonexistent/path.jsonl".to_string(),
    );

    let result = update_codex_prompt(Some(&metadata), "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Failed(_)));
}

#[test]
fn test_update_claude_prompt_invalid_path() {
    let mut metadata = HashMap::new();
    metadata.insert(
        "transcript_path".to_string(),
        "/nonexistent/path.jsonl".to_string(),
    );

    let result = update_claude_prompt(Some(&metadata), "claude-3");
    assert!(matches!(result, PromptUpdateResult::Failed(_)));
}

#[test]
fn test_update_gemini_prompt_invalid_path() {
    let mut metadata = HashMap::new();
    metadata.insert(
        "transcript_path".to_string(),
        "/nonexistent/path.json".to_string(),
    );

    let result = update_gemini_prompt(Some(&metadata), "gemini-pro");
    assert!(matches!(result, PromptUpdateResult::Failed(_)));
}

#[test]
fn test_update_github_copilot_prompt_invalid_path() {
    let mut metadata = HashMap::new();
    metadata.insert(
        "chat_session_path".to_string(),
        "/nonexistent/path.json".to_string(),
    );

    let result = update_github_copilot_prompt(Some(&metadata), "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Failed(_)));
}

#[test]
fn test_update_continue_cli_prompt_invalid_path() {
    let mut metadata = HashMap::new();
    metadata.insert(
        "transcript_path".to_string(),
        "/nonexistent/path.json".to_string(),
    );

    let result = update_continue_cli_prompt(Some(&metadata), "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Failed(_)));
}

#[test]
fn test_update_droid_prompt_invalid_transcript_path() {
    let mut metadata = HashMap::new();
    metadata.insert(
        "transcript_path".to_string(),
        "/nonexistent/path.jsonl".to_string(),
    );

    let result = update_droid_prompt(Some(&metadata), "gpt-4");
    assert!(matches!(result, PromptUpdateResult::Failed(_)));
}

#[test]
fn test_update_windsurf_prompt_no_metadata() {
    let result = update_windsurf_prompt(None, "unknown");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_windsurf_prompt_no_transcript_path() {
    let metadata = HashMap::new();
    let result = update_windsurf_prompt(Some(&metadata), "unknown");
    assert!(matches!(result, PromptUpdateResult::Unchanged));
}

#[test]
fn test_update_windsurf_prompt_invalid_path() {
    let mut metadata = HashMap::new();
    metadata.insert(
        "transcript_path".to_string(),
        "/nonexistent/path.jsonl".to_string(),
    );

    let result = update_windsurf_prompt(Some(&metadata), "unknown");
    assert!(matches!(result, PromptUpdateResult::Failed(_)));
}

#[test]
fn test_find_prompt_in_history_empty_repo() {
    let repo = TestRepo::new();

    // TestRepo always creates an initial commit, so we make one human commit
    // and then search for a bogus prompt ID - should still return error
    std::fs::write(repo.path().join("test.txt"), "content\n").unwrap();
    repo.stage_all_and_commit("Human commit").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let result = find_prompt_in_history(&gitai_repo, "any-prompt", 0);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Prompt not found in history")
    );
}

#[test]
fn test_find_prompt_prompt_not_in_commit() {
    let repo = TestRepo::new();

    // Create commit with AI checkpoint
    std::fs::write(repo.path().join("test.txt"), "content\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Test commit").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Try to find a different prompt ID
    let result = find_prompt_in_commit(&gitai_repo, "wrong-prompt-id", "HEAD");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Prompt 'wrong-prompt-id' not found in commit")
    );
}
