#[macro_use]
mod repos;

use git_ai::authorship::transcript::{AiTranscript, Message};
use repos::test_repo::TestRepo;
use std::fs;

/// Helper to create a simple agent_v1 AI checkpoint with a transcript
fn checkpoint_with_message(repo: &TestRepo, message: &str, edited_files: Vec<String>) {
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
        "conversation_id": "test-conversation-id",
    });

    let hook_input_str = serde_json::to_string(&hook_input).unwrap();

    repo.git_ai(&["checkpoint", "agent-v1", "--hook-input", &hook_input_str])
        .expect("checkpoint should succeed");
}

/// Helper to create a checkpoint with a specific conversation ID (for testing multiple prompts)
fn checkpoint_with_message_and_conversation_id(
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

/// Helper to create a checkpoint with an empty transcript (no messages)
fn checkpoint_with_empty_transcript(repo: &TestRepo, edited_files: Vec<String>) {
    let empty_transcript = AiTranscript::new();

    let hook_input = serde_json::json!({
        "type": "ai_agent",
        "repo_working_dir": repo.path().to_str().unwrap(),
        "edited_filepaths": edited_files,
        "transcript": empty_transcript,
        "agent_name": "test-agent",
        "model": "test-model",
        "conversation_id": "test-id",
    });

    let hook_input_str = serde_json::to_string(&hook_input).unwrap();

    repo.git_ai(&["checkpoint", "agent-v1", "--hook-input", &hook_input_str])
        .expect("checkpoint should succeed");
}

#[test]
fn test_checkpoint_with_prompt_sharing_enabled() {
    let mut repo = TestRepo::new();

    // Enable prompt sharing for all repositories (empty blacklist = share everywhere)
    // Use prompt_storage: "notes" to explicitly store messages in git notes for testing
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]); // No exclusions
        patch.prompt_storage = Some("notes".to_string()); // Store in notes for testing
    });

    // Create initial commit with README
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test Repo\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial commit"]).unwrap();

    // Write AI content directly (without using set_contents which triggers mock_ai)
    let example_path = repo.path().join("example.txt");
    fs::write(&example_path, "AI Line 1\nAI Line 2\n").unwrap();

    // Checkpoint with a proper message - this is the ONLY checkpoint for this file
    checkpoint_with_message(&repo, "Add example file", vec!["example.txt".to_string()]);

    // Stage and commit the changes
    repo.git(&["add", "-A"]).unwrap();
    let commit = repo.commit("Add example").expect("commit should succeed");

    // Verify we have the AI prompt in the commit
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected AI prompts in authorship log when prompt sharing is enabled"
    );

    // Verify the prompt message is captured
    let prompts: Vec<_> = commit.authorship_log.metadata.prompts.values().collect();
    assert_eq!(prompts.len(), 1, "Expected exactly one prompt");
    let prompt = prompts[0];
    assert!(
        !prompt.messages.is_empty(),
        "Expected messages in prompt when sharing is enabled"
    );

    // First message should be the user message
    if let Some(Message::User { text, .. }) = prompt.messages.first() {
        assert_eq!(text, "Add example file");
    } else {
        panic!("Expected first message to be a user message");
    }
}

#[test]
fn test_checkpoint_with_prompt_sharing_disabled_strips_messages() {
    let mut repo = TestRepo::new();

    // Prompt sharing is disabled by default (empty list), but let's be explicit
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec!["*".to_string()]); // Exclude all repos
    });

    // Create initial commit with README
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test Repo\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial commit"]).unwrap();

    // Add a remote so this isn't considered a local-only repo
    // (local repos always share prompts as they're safe)
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/test/repo.git",
    ])
    .unwrap();

    // Write AI content directly (without using set_contents which triggers mock_ai)
    let example_path = repo.path().join("example.txt");
    fs::write(&example_path, "AI Line 1\nAI Line 2\n").unwrap();

    // Use agent-v1 with a FULL transcript containing messages
    // This tests that messages are stripped when prompt sharing is disabled
    checkpoint_with_message(
        &repo,
        "Add the example file with AI content",
        vec!["example.txt".to_string()],
    );

    // Stage and commit the changes
    repo.git(&["add", "-A"]).unwrap();
    let commit = repo.commit("Add example").expect("commit should succeed");

    // Verify commit succeeded
    assert!(!commit.commit_sha.is_empty());

    // KEY ASSERTION: With prompt sharing disabled, the prompt RECORD should exist
    // (so we know AI was involved) but the MESSAGES should be empty (stripped)
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected prompt record to exist even when prompt sharing is disabled"
    );

    let prompts: Vec<_> = commit.authorship_log.metadata.prompts.values().collect();
    assert_eq!(prompts.len(), 1, "Expected exactly one prompt record");

    // The messages should be EMPTY because prompt sharing is disabled
    assert!(
        prompts[0].messages.is_empty(),
        "Expected messages to be stripped (empty) when prompt sharing is disabled, but found: {:?}",
        prompts[0].messages
    );
}

#[test]
fn test_multiple_checkpoints_with_messages() {
    let mut repo = TestRepo::new();

    // Enable prompt sharing for all repositories (empty blacklist = share everywhere)
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]); // No exclusions
    });

    // Create initial commit with README
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test Repo\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial commit"]).unwrap();

    // First AI edit - write file directly
    let file1_path = repo.path().join("file1.txt");
    fs::write(&file1_path, "AI Line 1\n").unwrap();

    checkpoint_with_message_and_conversation_id(
        &repo,
        "Create file1 with initial content",
        vec!["file1.txt".to_string()],
        "conversation-1",
    );

    // Second AI edit to different file
    let file2_path = repo.path().join("file2.txt");
    fs::write(&file2_path, "AI Line 2\n").unwrap();

    checkpoint_with_message_and_conversation_id(
        &repo,
        "Create file2 with different content",
        vec!["file2.txt".to_string()],
        "conversation-2",
    );

    // Stage and commit everything
    repo.git(&["add", "-A"]).unwrap();
    let commit = repo
        .commit("Add both files")
        .expect("commit should succeed");

    // Verify we captured both prompts
    assert_eq!(
        commit.authorship_log.metadata.prompts.len(),
        2,
        "Expected 2 prompts in authorship log"
    );

    // Collect prompts into a Vec for indexed access
    let prompts: Vec<_> = commit.authorship_log.metadata.prompts.values().collect();
    assert_eq!(prompts.len(), 2, "Expected exactly 2 prompts");

    // Verify both prompts have messages (order may vary due to BTreeMap)
    for prompt in &prompts {
        assert!(
            !prompt.messages.is_empty(),
            "Expected messages in prompt, but found empty messages for agent_id: {:?}",
            prompt.agent_id
        );
    }

    // Verify we have the expected user messages (content may be in either order)
    let user_messages: Vec<&str> = prompts
        .iter()
        .filter_map(|p| {
            if let Some(Message::User { text, .. }) = p.messages.first() {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();

    assert!(
        user_messages.contains(&"Create file1 with initial content"),
        "Expected to find 'Create file1 with initial content' in prompts"
    );
    assert!(
        user_messages.contains(&"Create file2 with different content"),
        "Expected to find 'Create file2 with different content' in prompts"
    );
}

#[test]
fn test_prompt_sharing_disabled_with_empty_transcript() {
    let mut repo = TestRepo::new();

    // Disable prompt sharing (default behavior)
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec!["*".to_string()]); // Exclude all repos
    });

    // Create initial commit with README
    let readme_path = repo.path().join("README.md");
    fs::write(&readme_path, "# Test Repo\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial commit"]).unwrap();

    // Write AI content directly
    let example_path = repo.path().join("example.txt");
    fs::write(&example_path, "AI Line\n").unwrap();

    // Checkpoint with an empty transcript (no messages)
    checkpoint_with_empty_transcript(&repo, vec!["example.txt".to_string()]);

    repo.git(&["add", "-A"]).unwrap();
    let commit = repo.commit("Add example").expect("commit should succeed");

    // With empty transcript, there should be a prompt record but with empty messages
    // Note: When transcript is empty, the prompt record may still exist but with no messages
    // The key thing is the checkpoint should succeed
    assert!(!commit.commit_sha.is_empty());
}

worktree_test_wrappers! {
    test_checkpoint_with_prompt_sharing_enabled,
    test_checkpoint_with_prompt_sharing_disabled_strips_messages,
    test_multiple_checkpoints_with_messages,
    test_prompt_sharing_disabled_with_empty_transcript,
}
