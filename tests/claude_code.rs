#[macro_use]
mod repos;
mod test_utils;

use git_ai::authorship::transcript::Message;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, ClaudePreset,
};
use serde_json::json;
use std::fs;
use std::io::Write;
use test_utils::fixture_path;

#[test]
fn test_parse_example_claude_code_jsonl_with_model() {
    let fixture = fixture_path("example-claude-code.jsonl");
    let (transcript, model) =
        ClaudePreset::transcript_and_model_from_claude_code_jsonl(fixture.to_str().unwrap())
            .expect("Failed to parse JSONL");

    // Verify we parsed some messages
    assert!(!transcript.messages().is_empty());

    // Verify we extracted the model
    assert!(model.is_some());
    let model_name = model.unwrap();
    println!("Extracted model: {}", model_name);

    // Based on the example file, we should get claude-sonnet-4-20250514
    assert_eq!(model_name, "claude-sonnet-4-20250514");

    // Print the parsed transcript for inspection
    println!("Parsed {} messages:", transcript.messages().len());
    for (i, message) in transcript.messages().iter().enumerate() {
        match message {
            Message::User { text, .. } => println!("{}: User: {}", i, text),
            Message::Assistant { text, .. } => println!("{}: Assistant: {}", i, text),
            Message::ToolUse { name, input, .. } => {
                println!("{}: ToolUse: {} with input: {:?}", i, name, input)
            }
            Message::Thinking { text, .. } => println!("{}: Thinking: {}", i, text),
            Message::Plan { text, .. } => println!("{}: Plan: {}", i, text),
        }
    }
}

#[test]
fn test_claude_preset_extracts_edited_filepath() {
    let hook_input = r##"{
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "permission_mode": "default",
        "session_id": "23aad27c-175d-427f-ac5f-a6830b8e6e65",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/README.md",
            "new_string": "# Testing Git Repository",
            "old_string": "# Testing Git"
        },
        "tool_name": "Edit",
        "transcript_path": "tests/fixtures/example-claude-code.jsonl"
    }"##;

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ClaudePreset;
    let result = preset.run(flags).expect("Failed to run ClaudePreset");

    // Verify edited_filepaths is extracted
    assert!(result.edited_filepaths.is_some());
    let edited_filepaths = result.edited_filepaths.unwrap();
    assert_eq!(edited_filepaths.len(), 1);
    assert_eq!(
        edited_filepaths[0],
        "/Users/svarlamov/projects/testing-git/README.md"
    );
}

#[test]
fn test_claude_preset_no_filepath_when_tool_input_missing() {
    let hook_input = r##"{
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "23aad27c-175d-427f-ac5f-a6830b8e6e65",
        "tool_name": "Read",
        "transcript_path": "tests/fixtures/example-claude-code.jsonl"
    }"##;

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ClaudePreset;
    let result = preset.run(flags).expect("Failed to run ClaudePreset");

    // Verify edited_filepaths is None when tool_input is missing
    assert!(result.edited_filepaths.is_none());
}

#[test]
fn test_claude_preset_redirects_vscode_copilot_payload() {
    let hook_input = json!({
        "hookEventName": "PreToolUse",
        "cwd": "/Users/test/project",
        "toolName": "copilot_replaceString",
        "toolInput": {
            "file_path": "/Users/test/project/src/main.ts"
        },
        "sessionId": "copilot-session-1",
        "model": "copilot/claude-sonnet-4"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ClaudePreset;
    let result = preset
        .run(flags)
        .expect("Expected redirect to Copilot preset");

    assert_eq!(
        result.checkpoint_kind,
        git_ai::authorship::working_log::CheckpointKind::Human
    );
    assert_eq!(result.agent_id.tool, "human");
    assert_eq!(
        result.will_edit_filepaths,
        Some(vec!["/Users/test/project/src/main.ts".to_string()])
    );
}

#[test]
fn test_claude_preset_does_not_redirect_when_transcript_path_is_claude() {
    let temp = tempfile::tempdir().unwrap();
    let claude_dir = temp.path().join(".claude").join("projects");
    fs::create_dir_all(&claude_dir).unwrap();

    let transcript_path = claude_dir.join("session.jsonl");
    let fixture = fixture_path("example-claude-code.jsonl");
    let mut dst = std::fs::File::create(&transcript_path).unwrap();
    let src = std::fs::read(fixture).unwrap();
    dst.write_all(&src).unwrap();

    let hook_input = json!({
        "hookEventName": "PostToolUse",
        "cwd": "/Users/test/project",
        "toolName": "copilot_replaceString",
        "toolInput": {
            "file_path": "/Users/test/project/src/main.ts"
        },
        "sessionId": "copilot-session-2",
        "transcript_path": transcript_path.to_string_lossy().to_string()
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ClaudePreset;
    let result = preset
        .run(flags)
        .expect("Expected native Claude preset handling");

    assert_eq!(result.agent_id.tool, "claude");
}

#[test]
fn test_claude_e2e_prefers_latest_checkpoint_for_prompts() {
    use repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();

    // Enable prompt sharing for all repositories (empty blacklist = no exclusions)
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]); // No exclusions = share everywhere
    });

    let repo_root = repo.canonical_path();

    // Create initial file and commit
    let src_dir = repo_root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("main.rs");
    fs::write(&file_path, "fn main() {}\n").unwrap();
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

    // First AI edit and checkpoint with empty transcript/model
    fs::write(&file_path, "fn main() {}\n// ai line one\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .unwrap();

    // Second AI edit with the real transcript content
    let fixture = fixture_path("example-claude-code.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();
    fs::write(&file_path, "fn main() {}\n// ai line one\n// ai line two\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .unwrap();

    // Commit the changes
    let commit = repo.stage_all_and_commit("Add AI lines").unwrap();

    // We should have exactly one prompt record keyed by the claude agent_id
    assert_eq!(
        commit.authorship_log.metadata.prompts.len(),
        1,
        "Expected a single prompt record"
    );
    let prompt_record = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Prompt record should exist");

    // The latest checkpoint (with the real transcript) should win
    assert!(
        !prompt_record.messages.is_empty(),
        "Prompt record should contain messages from the latest checkpoint"
    );
    assert_eq!(
        prompt_record.agent_id.model, "claude-sonnet-4-20250514",
        "Prompt record should use the model from the latest checkpoint transcript"
    );
}

#[test]
fn test_parse_claude_code_jsonl_with_thinking() {
    let fixture = fixture_path("claude-code-with-thinking.jsonl");
    let (transcript, model) =
        ClaudePreset::transcript_and_model_from_claude_code_jsonl(fixture.to_str().unwrap())
            .expect("Failed to parse JSONL");

    // Verify we parsed some messages
    assert!(!transcript.messages().is_empty());

    // Verify we extracted the model
    assert!(model.is_some());
    let model_name = model.unwrap();
    println!("Extracted model: {}", model_name);
    assert_eq!(model_name, "claude-sonnet-4-5-20250929");

    // Print the parsed transcript for inspection
    println!("Parsed {} messages:", transcript.messages().len());
    for (i, message) in transcript.messages().iter().enumerate() {
        match message {
            Message::User { text, .. } => {
                println!(
                    "{}: User: {}",
                    i,
                    text.chars().take(100).collect::<String>()
                )
            }
            Message::Assistant { text, .. } => {
                println!(
                    "{}: Assistant: {}",
                    i,
                    text.chars().take(100).collect::<String>()
                )
            }
            Message::ToolUse { name, input, .. } => {
                println!("{}: ToolUse: {} with input: {:?}", i, name, input)
            }
            Message::Thinking { text, .. } => {
                println!(
                    "{}: Thinking: {}",
                    i,
                    text.chars().take(100).collect::<String>()
                )
            }
            Message::Plan { text, .. } => {
                println!(
                    "{}: Plan: {}",
                    i,
                    text.chars().take(100).collect::<String>()
                )
            }
        }
    }

    // Verify message types and count
    // Expected messages (tool_result is skipped as it's not human-authored):
    // 1. User: "add another hello world console log to @index.ts "
    // 2. Assistant: thinking message (should be parsed as Assistant)
    // 3. Assistant: "I'll add another hello world console log to the file."
    // 4. ToolUse: Edit
    // 5. Assistant: thinking message (should be parsed as Assistant)
    // 6. Assistant: "Done! I've added another `console.log('hello world')` statement at index.ts:21."

    assert_eq!(
        transcript.messages().len(),
        6,
        "Expected 6 messages (1 user + 2 thinking + 2 text + 1 tool_use, tool_result skipped)"
    );

    // Check first message is User
    assert!(
        matches!(transcript.messages()[0], Message::User { .. }),
        "First message should be User"
    );

    // Check second message is Assistant (thinking)
    assert!(
        matches!(transcript.messages()[1], Message::Assistant { .. }),
        "Second message should be Assistant (thinking)"
    );
    if let Message::Assistant { text, .. } = &transcript.messages()[1] {
        assert!(
            text.contains("add another"),
            "Thinking message should contain thinking content"
        );
    }

    // Check third message is Assistant (text)
    assert!(
        matches!(transcript.messages()[2], Message::Assistant { .. }),
        "Third message should be Assistant (text)"
    );

    // Check fourth message is ToolUse
    assert!(
        matches!(transcript.messages()[3], Message::ToolUse { .. }),
        "Fourth message should be ToolUse"
    );
    if let Message::ToolUse { name, .. } = &transcript.messages()[3] {
        assert_eq!(name, "Edit", "Tool should be Edit");
    }

    // Check fifth message is Assistant (thinking) - tool_result was skipped
    assert!(
        matches!(transcript.messages()[4], Message::Assistant { .. }),
        "Fifth message should be Assistant (thinking)"
    );

    // Check sixth message is Assistant (text)
    assert!(
        matches!(transcript.messages()[5], Message::Assistant { .. }),
        "Sixth message should be Assistant (text)"
    );
}

#[test]
fn test_tool_results_are_not_parsed_as_user_messages() {
    // This test verifies that tool_result content blocks in user messages
    // are not incorrectly parsed as human-authored user messages.
    // Tool results are system-generated responses to tool calls, not human input.

    use std::io::Write;
    use tempfile::NamedTempFile;

    // Create a JSONL with a user message containing only a tool_result
    let jsonl_content = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_123","type":"tool_result","content":"File created successfully"}]},"timestamp":"2025-01-01T00:00:00Z"}
{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"text","text":"Done!"}]},"timestamp":"2025-01-01T00:00:01Z"}"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let (transcript, _model) = ClaudePreset::transcript_and_model_from_claude_code_jsonl(temp_path)
        .expect("Failed to parse JSONL");

    // Should only have 1 message (the assistant response)
    // The tool_result should be skipped entirely
    assert_eq!(
        transcript.messages().len(),
        1,
        "Tool results should not be parsed as user messages"
    );

    // The only message should be the assistant response
    assert!(
        matches!(transcript.messages()[0], Message::Assistant { .. }),
        "Only message should be Assistant"
    );
    if let Message::Assistant { text, .. } = &transcript.messages()[0] {
        assert_eq!(text, "Done!");
    }
}

#[test]
fn test_user_text_content_blocks_are_parsed_correctly() {
    // This test verifies that user messages with text content blocks
    // (as opposed to simple string content) are parsed correctly.

    use std::io::Write;
    use tempfile::NamedTempFile;

    // Create a JSONL with a user message containing a text content block
    let jsonl_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello, can you help me?"}]},"timestamp":"2025-01-01T00:00:00Z"}
{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"text","text":"Of course!"}]},"timestamp":"2025-01-01T00:00:01Z"}"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let (transcript, _model) = ClaudePreset::transcript_and_model_from_claude_code_jsonl(temp_path)
        .expect("Failed to parse JSONL");

    // Should have 2 messages (user + assistant)
    assert_eq!(
        transcript.messages().len(),
        2,
        "Should have user and assistant messages"
    );

    // First message should be User with the correct text
    assert!(
        matches!(transcript.messages()[0], Message::User { .. }),
        "First message should be User"
    );
    if let Message::User { text, .. } = &transcript.messages()[0] {
        assert_eq!(text, "Hello, can you help me?");
    }

    // Second message should be Assistant
    assert!(
        matches!(transcript.messages()[1], Message::Assistant { .. }),
        "Second message should be Assistant"
    );
}
