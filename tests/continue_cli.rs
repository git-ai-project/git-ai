#[macro_use]
mod repos;
mod test_utils;

use git_ai::authorship::transcript::Message;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, ContinueCliPreset,
};
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use serde_json::json;
use std::fs;
use std::io::Write;
use test_utils::fixture_path;

#[test]
fn test_parse_example_continue_cli_json() {
    let fixture = fixture_path("continue-cli-session-simple.json");
    let transcript = ContinueCliPreset::transcript_from_continue_json(fixture.to_str().unwrap())
        .expect("Failed to parse Continue CLI JSON");

    // Verify we parsed some messages
    assert!(!transcript.messages().is_empty());

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
fn test_continue_cli_parses_user_messages() {
    let fixture = fixture_path("continue-cli-session-simple.json");
    let transcript = ContinueCliPreset::transcript_from_continue_json(fixture.to_str().unwrap())
        .expect("Failed to parse Continue CLI JSON");

    // Find user messages
    let user_messages: Vec<&Message> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::User { .. }))
        .collect::<Vec<_>>();

    assert_eq!(
        user_messages.len(),
        1,
        "Should have exactly one user message"
    );

    // Verify the user message content
    if let Message::User { text, .. } = user_messages[0] {
        assert!(text.contains("Add another hello world line"));
    }
}

#[test]
fn test_continue_cli_parses_assistant_messages() {
    let fixture = fixture_path("continue-cli-session-simple.json");
    let transcript = ContinueCliPreset::transcript_from_continue_json(fixture.to_str().unwrap())
        .expect("Failed to parse Continue CLI JSON");

    // Find assistant messages
    let assistant_messages: Vec<&Message> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::Assistant { .. }))
        .collect();

    assert!(
        !assistant_messages.is_empty(),
        "Should have at least one assistant message"
    );

    // Verify the first assistant message has content
    if let Message::Assistant { text, .. } = assistant_messages[0] {
        assert!(text.contains("I'll read the file first"));
    }
}

#[test]
fn test_continue_cli_parses_tool_calls() {
    let fixture = fixture_path("continue-cli-session-simple.json");
    let transcript = ContinueCliPreset::transcript_from_continue_json(fixture.to_str().unwrap())
        .expect("Failed to parse Continue CLI JSON");

    // Find tool use messages
    let tool_uses: Vec<&Message> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::ToolUse { .. }))
        .collect();

    assert!(!tool_uses.is_empty(), "Should have at least one tool call");

    // Verify tool calls have correct structure
    for tool_use in &tool_uses {
        if let Message::ToolUse { name, input, .. } = tool_use {
            assert!(!name.is_empty());
            // Verify input is a JSON object
            assert!(input.is_object());
        }
    }

    // Check for specific tool calls from the fixture
    let read_tools: Vec<&Message> = tool_uses
        .iter()
        .filter(|m| {
            if let Message::ToolUse { name, .. } = *m {
                name == "Read"
            } else {
                false
            }
        })
        .copied()
        .collect();

    assert!(
        !read_tools.is_empty(),
        "Should have at least one 'Read' tool call"
    );
}

#[test]
fn test_continue_cli_parses_tool_call_args() {
    let fixture = fixture_path("continue-cli-session-simple.json");
    let transcript = ContinueCliPreset::transcript_from_continue_json(fixture.to_str().unwrap())
        .expect("Failed to parse Continue CLI JSON");

    // Find a Read tool call
    let read_tool = transcript
        .messages()
        .iter()
        .find(|m| {
            if let Message::ToolUse { name, .. } = m {
                name == "Read"
            } else {
                false
            }
        })
        .expect("Should find a Read tool call");

    if let Message::ToolUse { input, .. } = read_tool {
        // Verify args structure
        if let Some(args_obj) = input.as_object() {
            // Check for expected fields
            assert!(
                args_obj.contains_key("filepath"),
                "Tool call args should contain filepath"
            );
        }
    }
}

#[test]
fn test_continue_cli_handles_empty_content() {
    // Test that empty content strings are skipped
    let sample = r##"{
        "sessionId": "test-session",
        "title": "Test",
        "workspaceDirectory": "/test",
        "history": [
            {
                "message": {
                    "role": "user",
                    "content": "Hello"
                },
                "contextItems": []
            },
            {
                "message": {
                    "role": "assistant",
                    "content": ""
                },
                "contextItems": []
            },
            {
                "message": {
                    "role": "assistant",
                    "content": "Response text"
                },
                "contextItems": []
            }
        ]
    }"##;

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let transcript = ContinueCliPreset::transcript_from_continue_json(temp_path)
        .expect("Failed to parse Continue CLI JSON");

    // Should have 1 user message and 1 assistant message (empty content skipped)
    let user_count = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::User { .. }))
        .count();
    let assistant_count = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::Assistant { .. }))
        .count();

    assert_eq!(user_count, 1);
    assert_eq!(assistant_count, 1, "Should skip empty content");
}

#[test]
fn test_continue_cli_preset_extracts_model_from_hook_input() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ContinueCliPreset;
    let result = preset.run(flags).expect("Failed to run ContinueCliPreset");

    // Verify model is extracted from hook_input
    assert_eq!(result.agent_id.model, "claude-3.5-sonnet");
    assert_eq!(result.agent_id.tool, "continue-cli");
    assert_eq!(result.agent_id.id, "2dbfd673-096d-4773-b5f3-9023894a7355");
}

#[test]
fn test_continue_cli_preset_defaults_to_unknown_model() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ContinueCliPreset;
    let result = preset.run(flags).expect("Failed to run ContinueCliPreset");

    // Verify model defaults to "unknown" when not provided
    assert_eq!(result.agent_id.model, "unknown");
}

#[test]
fn test_continue_cli_preset_extracts_edited_filepath() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ContinueCliPreset;
    let result = preset.run(flags).expect("Failed to run ContinueCliPreset");

    // Verify edited_filepaths is extracted
    assert!(result.edited_filepaths.is_some());
    let edited_filepaths = result.edited_filepaths.unwrap();
    assert_eq!(edited_filepaths.len(), 1);
    assert_eq!(
        edited_filepaths[0],
        "/Users/svarlamov/projects/testing-git/index.ts"
    );
}

#[test]
fn test_continue_cli_preset_no_filepath_when_tool_input_missing() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ContinueCliPreset;
    let result = preset.run(flags).expect("Failed to run ContinueCliPreset");

    // Verify edited_filepaths is None when tool_input is missing
    assert!(result.edited_filepaths.is_none());
}

#[test]
fn test_continue_cli_preset_human_checkpoint() {
    use git_ai::authorship::working_log::CheckpointKind;

    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PreToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ContinueCliPreset;
    let result = preset.run(flags).expect("Failed to run ContinueCliPreset");

    // Verify this is a human checkpoint
    assert_eq!(
        result.checkpoint_kind,
        CheckpointKind::Human,
        "Should be a human checkpoint"
    );

    // Human checkpoints should have will_edit_filepaths
    assert!(result.will_edit_filepaths.is_some());
    let will_edit = result.will_edit_filepaths.unwrap();
    assert_eq!(will_edit.len(), 1);
    assert_eq!(
        will_edit[0],
        "/Users/svarlamov/projects/testing-git/index.ts"
    );

    // Human checkpoints should not have edited_filepaths
    assert!(result.edited_filepaths.is_none());

    // Human checkpoints should not have transcript
    assert!(result.transcript.is_none());
}

#[test]
fn test_continue_cli_preset_ai_checkpoint() {
    use git_ai::authorship::working_log::CheckpointKind;

    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ContinueCliPreset;
    let result = preset.run(flags).expect("Failed to run ContinueCliPreset");

    // Verify this is an AI checkpoint
    assert_eq!(
        result.checkpoint_kind,
        CheckpointKind::AiAgent,
        "Should be an AI agent checkpoint"
    );

    // AI checkpoints should have transcript
    assert!(result.transcript.is_some());

    // AI checkpoints should have edited_filepaths
    assert!(result.edited_filepaths.is_some());

    // AI checkpoints should not have will_edit_filepaths
    assert!(result.will_edit_filepaths.is_none());
}

#[test]
fn test_continue_cli_preset_stores_transcript_path_in_metadata() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ContinueCliPreset;
    let result = preset.run(flags).expect("Failed to run ContinueCliPreset");

    // Verify transcript_path is stored in metadata
    assert!(result.agent_metadata.is_some());
    let metadata = result.agent_metadata.unwrap();
    assert_eq!(
        metadata.get("transcript_path"),
        Some(&"tests/fixtures/continue-cli-session-simple.json".to_string())
    );
}

#[test]
fn test_continue_cli_preset_handles_missing_transcript_path() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ContinueCliPreset;
    let result = preset.run(flags);

    // Should fail because transcript_path is required
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("transcript_path not found")
    );
}

#[test]
fn test_continue_cli_preset_handles_invalid_json() {
    let hook_input = "{ invalid json }";

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ContinueCliPreset;
    let result = preset.run(flags);

    // Should fail because JSON is invalid
    assert!(result.is_err());
}

#[test]
fn test_continue_cli_preset_handles_missing_session_id() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "model": "claude-3.5-sonnet",
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ContinueCliPreset;
    let result = preset.run(flags);

    // Should fail because session_id is required
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("session_id not found")
    );
}

#[test]
fn test_continue_cli_preset_handles_missing_file() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "transcript_path": "tests/fixtures/nonexistent.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = ContinueCliPreset;
    let result = preset.run(flags);

    // Should handle missing file gracefully (returns empty transcript)
    assert!(result.is_ok());
    let run_result = result.unwrap();
    // The preset should handle this gracefully
    assert_eq!(run_result.agent_id.model, "claude-3.5-sonnet");
}

// ============================================================================
// End-to-end tests using TestRepo
// ============================================================================

#[test]
fn test_continue_cli_e2e_with_attribution() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("continue-cli-session-simple.json")
        .to_string_lossy()
        .to_string();

    // Create parent directory for the test file
    let src_dir = repo.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create initial file with some base content
    let file_path = repo.path().join("src/index.ts");
    let base_content = "console.log('Bonjour');\n\nconsole.log('hello world');\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Simulate Continue CLI making edits to the file
    let edited_content =
        "console.log('Bonjour');\n\nconsole.log('hello world');\nconsole.log('hello world');\n";
    fs::write(&file_path, edited_content).unwrap();

    // Run checkpoint with the Continue CLI session
    let hook_input = json!({
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    let result = repo
        .git_ai(&["checkpoint", "continue-cli", "--hook-input", &hook_input])
        .unwrap();

    println!("Checkpoint output: {}", result);

    // Commit the changes
    let commit = repo.stage_all_and_commit("Add continue-cli edits").unwrap();

    // Verify attribution using TestFile
    let mut file = repo.filename("src/index.ts");
    file.assert_lines_and_blame(lines![
        "console.log('Bonjour');".human(),
        "".human(),
        "console.log('hello world');".human(),
        "console.log('hello world');".ai(),
    ]);

    // Verify the authorship log contains attestations and prompts
    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Should have at least one attestation"
    );

    // Verify the metadata has prompts with transcript data
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Should have at least one prompt record in metadata"
    );

    // Get the first prompt record
    let prompt_record = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Should have at least one prompt record");

    // Verify that the prompt record has messages (transcript)
    assert!(
        !prompt_record.messages.is_empty(),
        "Prompt record should contain messages from the continue-cli session"
    );

    // Verify the model was preserved correctly
    assert_eq!(
        prompt_record.agent_id.model, "claude-3.5-sonnet",
        "Model should be 'claude-3.5-sonnet'"
    );
}

#[test]
fn test_continue_cli_e2e_human_checkpoint() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("continue-cli-session-simple.json")
        .to_string_lossy()
        .to_string();

    // Create parent directory for the test file
    let src_dir = repo.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create initial file
    let file_path = repo.path().join("src/index.ts");
    let base_content = "console.log('hello');\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Human checkpoint before tool use
    let hook_input = json!({
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    let result = repo
        .git_ai(&["checkpoint", "continue-cli", "--hook-input", &hook_input])
        .unwrap();

    println!("Checkpoint output: {}", result);

    // Make a human edit
    let human_content = "console.log('hello');\nconsole.log('human edit');\n";
    fs::write(&file_path, human_content).unwrap();

    // Commit the changes
    let commit = repo.stage_all_and_commit("Human edit").unwrap();

    // Verify attribution - human edit should be human
    let mut file = repo.filename("src/index.ts");
    file.assert_lines_and_blame(lines![
        "console.log('hello');".human(),
        "console.log('human edit');".human(),
    ]);

    // Human checkpoint should not create AI attestations
    assert_eq!(
        commit.authorship_log.attestations.len(),
        0,
        "Human checkpoint should not create AI attestations"
    );
}

#[test]
fn test_continue_cli_e2e_multiple_tool_calls() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("continue-cli-session-simple.json")
        .to_string_lossy()
        .to_string();

    // Create initial file
    let file_path = repo.path().join("test.ts");
    let base_content = "const x = 1;\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make edits
    let edited_content = "const x = 1;\nconst y = 2;\nconst z = 3;\n";
    fs::write(&file_path, edited_content).unwrap();

    // Run checkpoint
    let hook_input = json!({
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    repo.git_ai(&["checkpoint", "continue-cli", "--hook-input", &hook_input])
        .unwrap();

    // Commit
    let commit = repo.stage_all_and_commit("Add multiple lines").unwrap();

    // Verify attribution
    let mut file = repo.filename("test.ts");
    file.assert_lines_and_blame(lines![
        "const x = 1;".human(),
        "const y = 2;".ai(),
        "const z = 3;".ai(),
    ]);

    assert!(!commit.authorship_log.attestations.is_empty());
}

#[test]
fn test_continue_cli_e2e_preserves_model_on_commit() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("continue-cli-session-simple.json")
        .to_string_lossy()
        .to_string();

    // Create initial file
    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make edits
    fs::write(&file_path, "const x = 1;\nconst y = 2;\n").unwrap();

    // Run checkpoint with a specific model
    let hook_input = json!({
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "model": "claude-opus-4",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    repo.git_ai(&["checkpoint", "continue-cli", "--hook-input", &hook_input])
        .unwrap();

    // Commit
    let commit = repo.stage_all_and_commit("Add line").unwrap();

    // Verify the model was preserved (not overwritten by post-commit)
    let prompt_record = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Should have a prompt record");

    assert_eq!(
        prompt_record.agent_id.model, "claude-opus-4",
        "Model should be preserved from hook_input"
    );
    assert_eq!(prompt_record.agent_id.tool, "continue-cli");
}

reuse_tests_in_worktree!(
    test_parse_example_continue_cli_json,
    test_continue_cli_parses_user_messages,
    test_continue_cli_parses_assistant_messages,
    test_continue_cli_parses_tool_calls,
    test_continue_cli_parses_tool_call_args,
    test_continue_cli_handles_empty_content,
    test_continue_cli_preset_extracts_model_from_hook_input,
    test_continue_cli_preset_defaults_to_unknown_model,
    test_continue_cli_preset_extracts_edited_filepath,
    test_continue_cli_preset_no_filepath_when_tool_input_missing,
    test_continue_cli_preset_human_checkpoint,
    test_continue_cli_preset_ai_checkpoint,
    test_continue_cli_preset_stores_transcript_path_in_metadata,
    test_continue_cli_preset_handles_missing_transcript_path,
    test_continue_cli_preset_handles_invalid_json,
    test_continue_cli_preset_handles_missing_session_id,
    test_continue_cli_preset_handles_missing_file,
    test_continue_cli_e2e_with_attribution,
    test_continue_cli_e2e_human_checkpoint,
    test_continue_cli_e2e_multiple_tool_calls,
    test_continue_cli_e2e_preserves_model_on_commit,
);
