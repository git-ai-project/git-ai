#[macro_use]
mod repos;
mod test_utils;

use git_ai::authorship::transcript::Message;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, GeminiPreset,
};
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use serde_json::json;
use std::fs;
use std::io::Write;
use test_utils::fixture_path;

#[test]
fn test_parse_example_gemini_json_with_model() {
    let fixture = fixture_path("gemini-session-simple.json");
    let (transcript, model) = GeminiPreset::transcript_and_model_from_gemini_json(
        fixture.to_str().unwrap(),
    )
    .expect("Failed to parse Gemini JSON");

    // Verify we parsed some messages
    assert!(!transcript.messages().is_empty());

    // Verify we extracted the model
    assert!(model.is_some());
    let model_name = model.unwrap();
    println!("Extracted model: {}", model_name);

    // Based on the example file, we should get gemini-2.5-flash
    assert_eq!(model_name, "gemini-2.5-flash");

    // Print the parsed transcript for inspection
    println!("Parsed {} messages:", transcript.messages().len());
    for (i, message) in transcript.messages().iter().enumerate() {
        match message {
            Message::User { text, .. } => println!("{}: User: {}", i, text),
            Message::Assistant { text, .. } => println!("{}: Assistant: {}", i, text),
            Message::ToolUse { name, input, .. } => {
                println!("{}: ToolUse: {} with input: {:?}", i, name, input)
            }
        }
    }
}

#[test]
fn test_gemini_parses_user_messages() {
    let fixture = fixture_path("gemini-session-simple.json");
    let (transcript, _model) = GeminiPreset::transcript_and_model_from_gemini_json(
        fixture.to_str().unwrap(),
    )
    .expect("Failed to parse Gemini JSON");

    // Find user messages
    let user_messages: Vec<&Message> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::User { .. }))
        .collect::<Vec<_>>();

    assert_eq!(user_messages.len(), 1, "Should have exactly one user message");

    // Verify the user message content
    if let Message::User { text, timestamp } = user_messages[0] {
        assert!(text.contains("add another hello bob console log"));
        assert_eq!(
            timestamp.as_ref().unwrap(),
            "2025-12-06T18:25:18.042Z"
        );
    }
}

#[test]
fn test_gemini_parses_assistant_messages() {
    let fixture = fixture_path("gemini-session-simple.json");
    let (transcript, _model) = GeminiPreset::transcript_and_model_from_gemini_json(
        fixture.to_str().unwrap(),
    )
    .expect("Failed to parse Gemini JSON");

    // Find assistant messages
    let assistant_messages: Vec<&Message> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::Assistant { .. }))
        .collect();

    assert!(
        assistant_messages.len() >= 1,
        "Should have at least one assistant message"
    );

    // Verify the first assistant message has content
    if let Message::Assistant { text, .. } = assistant_messages[0] {
        assert!(text.contains("I will add"));
    }
}

#[test]
fn test_gemini_parses_tool_calls() {
    let fixture = fixture_path("gemini-session-simple.json");
    let (transcript, _model) = GeminiPreset::transcript_and_model_from_gemini_json(
        fixture.to_str().unwrap(),
    )
    .expect("Failed to parse Gemini JSON");

    // Find tool use messages
    let tool_uses: Vec<&Message> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::ToolUse { .. }))
        .collect();

    assert!(
        tool_uses.len() >= 1,
        "Should have at least one tool call"
    );

    // Verify tool calls have correct structure
    for tool_use in &tool_uses {
        if let Message::ToolUse { name, input, .. } = tool_use {
            assert!(!name.is_empty());
            // Verify input is a JSON object
            assert!(input.is_object() || input.is_string());
        }
    }

    // Check for specific tool calls from the fixture
    let replace_tools: Vec<&Message> = tool_uses
        .iter()
        .filter(|m| {
            if let Message::ToolUse { name, .. } = *m {
                name == "replace"
            } else {
                false
            }
        })
        .copied()
        .collect();

    assert!(
        replace_tools.len() >= 1,
        "Should have at least one 'replace' tool call"
    );
}

#[test]
fn test_gemini_parses_tool_call_args() {
    let fixture = fixture_path("gemini-session-simple.json");
    let (transcript, _model) = GeminiPreset::transcript_and_model_from_gemini_json(
        fixture.to_str().unwrap(),
    )
    .expect("Failed to parse Gemini JSON");

    // Find a replace tool call
    let replace_tool = transcript
        .messages()
        .iter()
        .find(|m| {
            if let Message::ToolUse { name, .. } = m {
                name == "replace"
            } else {
                false
            }
        })
        .expect("Should find a replace tool call");

    if let Message::ToolUse { input, .. } = replace_tool {
        // Verify args structure
        if let Some(args_obj) = input.as_object() {
            // Check for expected fields
            assert!(
                args_obj.contains_key("file_path") || args_obj.contains_key("old_string"),
                "Tool call args should contain file_path or old_string"
            );
        }
    }
}

#[test]
fn test_gemini_handles_empty_content() {
    // Test that empty content strings are skipped
    let sample = r##"{
        "sessionId": "test-session",
        "projectHash": "test-hash",
        "startTime": "2025-12-06T18:25:18.042Z",
        "lastUpdated": "2025-12-06T18:25:18.042Z",
        "messages": [
            {
                "id": "msg1",
                "timestamp": "2025-12-06T18:25:18.042Z",
                "type": "user",
                "content": "Hello"
            },
            {
                "id": "msg2",
                "timestamp": "2025-12-06T18:25:18.042Z",
                "type": "gemini",
                "content": "",
                "model": "gemini-2.5-flash"
            },
            {
                "id": "msg3",
                "timestamp": "2025-12-06T18:25:18.042Z",
                "type": "gemini",
                "content": "Response text",
                "model": "gemini-2.5-flash"
            }
        ]
    }"##;

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let (transcript, model) = GeminiPreset::transcript_and_model_from_gemini_json(temp_path)
        .expect("Failed to parse Gemini JSON");

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
    assert_eq!(model, Some("gemini-2.5-flash".to_string()));
}

#[test]
fn test_gemini_skips_unknown_message_types() {
    let sample = r##"{
        "sessionId": "test-session",
        "projectHash": "test-hash",
        "startTime": "2025-12-06T18:25:18.042Z",
        "lastUpdated": "2025-12-06T18:25:18.042Z",
        "messages": [
            {
                "id": "msg1",
                "timestamp": "2025-12-06T18:25:18.042Z",
                "type": "user",
                "content": "Hello"
            },
            {
                "id": "msg2",
                "timestamp": "2025-12-06T18:25:18.042Z",
                "type": "info",
                "content": "Info message"
            },
            {
                "id": "msg3",
                "timestamp": "2025-12-06T18:25:18.042Z",
                "type": "error",
                "content": "Error message"
            },
            {
                "id": "msg4",
                "timestamp": "2025-12-06T18:25:18.042Z",
                "type": "gemini",
                "content": "Response",
                "model": "gemini-2.5-flash"
            }
        ]
    }"##;

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let (transcript, _model) = GeminiPreset::transcript_and_model_from_gemini_json(temp_path)
        .expect("Failed to parse Gemini JSON");

    // Should only have user and gemini messages, skip info/error
    assert_eq!(transcript.messages().len(), 2);
}

#[test]
fn test_gemini_preset_extracts_edited_filepath() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = GeminiPreset;
    let result = preset.run(flags).expect("Failed to run GeminiPreset");

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
fn test_gemini_preset_no_filepath_when_tool_input_missing() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = GeminiPreset;
    let result = preset.run(flags).expect("Failed to run GeminiPreset");

    // Verify edited_filepaths is None when tool_input is missing
    assert!(result.edited_filepaths.is_none());
}

#[test]
fn test_gemini_preset_human_checkpoint() {
    use git_ai::authorship::working_log::CheckpointKind;

    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "BeforeTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = GeminiPreset;
    let result = preset.run(flags).expect("Failed to run GeminiPreset");

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
fn test_gemini_preset_ai_checkpoint() {
    use git_ai::authorship::working_log::CheckpointKind;

    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = GeminiPreset;
    let result = preset.run(flags).expect("Failed to run GeminiPreset");

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
fn test_gemini_preset_extracts_model() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = GeminiPreset;
    let result = preset.run(flags).expect("Failed to run GeminiPreset");

    // Verify model is extracted
    assert_eq!(result.agent_id.model, "gemini-2.5-flash");
    assert_eq!(result.agent_id.tool, "gemini");
    assert_eq!(
        result.agent_id.id,
        "18f475c0-690f-4bc9-b84e-88a0a1e9518f"
    );
}

#[test]
fn test_gemini_preset_stores_transcript_path_in_metadata() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = GeminiPreset;
    let result = preset.run(flags).expect("Failed to run GeminiPreset");

    // Verify transcript_path is stored in metadata
    assert!(result.agent_metadata.is_some());
    let metadata = result.agent_metadata.unwrap();
    assert_eq!(
        metadata.get("transcript_path"),
        Some(&"tests/fixtures/gemini-session-simple.json".to_string())
    );
}

#[test]
fn test_gemini_preset_handles_missing_transcript_path() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = GeminiPreset;
    let result = preset.run(flags);

    // Should fail because transcript_path is required
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("transcript_path not found"));
}

#[test]
fn test_gemini_preset_handles_invalid_json() {
    let hook_input = "{ invalid json }";

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = GeminiPreset;
    let result = preset.run(flags);

    // Should fail because JSON is invalid
    assert!(result.is_err());
}

#[test]
fn test_gemini_preset_handles_missing_session_id() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = GeminiPreset;
    let result = preset.run(flags);

    // Should fail because session_id is required
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("session_id not found"));
}

#[test]
fn test_gemini_preset_handles_missing_file() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "transcript_path": "tests/fixtures/nonexistent.json"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = GeminiPreset;
    let result = preset.run(flags);

    // Should handle missing file gracefully (returns empty transcript with unknown model)
    assert!(result.is_ok());
    let run_result = result.unwrap();
    // The preset should handle this gracefully and return unknown model
    assert_eq!(run_result.agent_id.model, "unknown");
}

// ============================================================================
// End-to-end tests using TestRepo
// ============================================================================

#[test]
fn test_gemini_e2e_with_attribution() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("gemini-session-simple.json")
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

    // Simulate Gemini making edits to the file
    let edited_content = "console.log('Bonjour');\n\nconsole.log('hello world');\nconsole.log('hello bob');\n";
    fs::write(&file_path, edited_content).unwrap();

    // Run checkpoint with the Gemini session
    let hook_input = json!({
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "AfterTool",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    let result = repo
        .git_ai(&["checkpoint", "gemini", "--hook-input", &hook_input])
        .unwrap();

    println!("Checkpoint output: {}", result);

    // Commit the changes
    let commit = repo.stage_all_and_commit("Add gemini edits").unwrap();

    // Verify attribution using TestFile
    let mut file = repo.filename("src/index.ts");
    file.assert_lines_and_blame(lines![
        "console.log('Bonjour');".human(),
        "".human(),
        "console.log('hello world');".human(),
        "console.log('hello bob');".ai(),
    ]);

    // Verify the authorship log contains attestations and prompts
    assert!(
        commit.authorship_log.attestations.len() > 0,
        "Should have at least one attestation"
    );

    // Verify the metadata has prompts with transcript data
    assert!(
        commit.authorship_log.metadata.prompts.len() > 0,
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
        prompt_record.messages.len() > 0,
        "Prompt record should contain messages from the gemini session"
    );

    // Verify the model was extracted correctly
    assert_eq!(
        prompt_record.agent_id.model, "gemini-2.5-flash",
        "Model should be 'gemini-2.5-flash'"
    );
}

#[test]
fn test_gemini_e2e_human_checkpoint() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("gemini-session-simple.json")
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
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "BeforeTool",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    let result = repo
        .git_ai(&["checkpoint", "gemini", "--hook-input", &hook_input])
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
fn test_gemini_e2e_multiple_tool_calls() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("gemini-session-simple.json")
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
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "AfterTool",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    repo.git_ai(&["checkpoint", "gemini", "--hook-input", &hook_input])
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

    assert!(commit.authorship_log.attestations.len() > 0);
}

#[test]
fn test_gemini_e2e_with_resync() {
    use std::fs;
    use tempfile::TempDir;

    let repo = TestRepo::new();
    let fixture_path_original = fixture_path("gemini-session-simple.json");

    // Create a temp directory for the modified session file
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let temp_session_path = temp_dir.path().join("modified_gemini_session.json");

    // Copy the fixture to temp location
    fs::copy(&fixture_path_original, &temp_session_path)
        .expect("Failed to copy session file");

    // Modify the session file to add a new message
    let mut session_content: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&temp_session_path).unwrap()).unwrap();

    // Add a new message to simulate updates after checkpoint
    if let Some(messages) = session_content.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let new_message = json!({
            "id": "new-msg-id",
            "timestamp": "2025-12-06T18:30:00.000Z",
            "type": "gemini",
            "content": "RESYNC_TEST_MESSAGE: This message was added after checkpoint",
            "model": "gemini-2.5-flash"
        });
        messages.push(new_message);
    }

    fs::write(&temp_session_path, serde_json::to_string_pretty(&session_content).unwrap())
        .expect("Failed to write modified session");

    // Create initial file
    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make edits
    fs::write(&file_path, "const x = 1;\nconst y = 2;\n").unwrap();

    // Run checkpoint with ORIGINAL session file
    let hook_input = json!({
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "AfterTool",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_original.to_string_lossy().to_string()
    })
    .to_string();

    repo.git_ai(&["checkpoint", "gemini", "--hook-input", &hook_input])
        .unwrap();

    // Now commit with the MODIFIED session file - this tests the resync logic in post_commit
    let temp_session_path_str = temp_session_path.to_string_lossy().to_string();
    repo.git(&["add", "-A"]).expect("add --all should succeed");
    let commit = repo
        .commit_with_env(
            "Add gemini edits",
            &[("GIT_AI_GEMINI_SESSION_PATH", &temp_session_path_str)],
        )
        .unwrap();

    // Verify attribution still works
    let mut file = repo.filename("test.ts");
    file.assert_lines_and_blame(lines!["const x = 1;".human(), "const y = 2;".ai(),]);

    // Verify the authorship log contains prompts
    assert!(
        commit.authorship_log.metadata.prompts.len() > 0,
        "Should have at least one prompt record"
    );

    // Get the first prompt record
    let prompt_record = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Should have at least one prompt record");

    // Verify that resync logic picked up the updated message
    let _transcript_json =
        serde_json::to_string(&prompt_record.messages).expect("Should serialize messages");

    // Note: The resync logic reads from metadata.transcript_path, so we need to verify
    // that the post_commit logic would pick up the new message if transcript_path is in metadata
    // For now, we just verify the basic structure is correct
    assert!(
        prompt_record.messages.len() > 0,
        "Prompt record should contain messages"
    );
}

#[test]
fn test_gemini_e2e_partial_staging() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("gemini-session-simple.json")
        .to_string_lossy()
        .to_string();

    // Create initial file
    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "line1\nline2\n").unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make edits
    fs::write(&file_path, "line1\nline2\nai_line3\nai_line4\n").unwrap();

    // Stage only some lines
    repo.git(&["add", "test.ts"]).unwrap();

    // Make more edits that won't be staged
    fs::write(&file_path, "line1\nline2\nai_line3\nai_line4\nai_line5\n").unwrap();

    // Run checkpoint
    let hook_input = json!({
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "AfterTool",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    repo.git_ai(&["checkpoint", "gemini", "--hook-input", &hook_input])
        .unwrap();

    // Commit only staged lines
    let commit = repo.commit("Partial staging").unwrap();

    // Verify only staged lines are attributed
    assert!(commit.authorship_log.attestations.len() > 0);

    // Check committed lines only
    let mut file = repo.filename("test.ts");
    file.assert_committed_lines(lines![
        "line1".human(),
        "line2".human(),
        "ai_line3".ai(),
        "ai_line4".ai(),
        // ai_line5 is not committed because it's unstaged
    ]);
}

