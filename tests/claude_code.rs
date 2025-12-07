mod test_utils;

use git_ai::authorship::transcript::Message;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, ClaudePreset,
};
use test_utils::fixture_path;

#[test]
fn test_parse_example_claude_code_jsonl_with_model() {
    let fixture = fixture_path("example-claude-code.jsonl");
    let (transcript, model) = ClaudePreset::transcript_and_model_from_claude_code_jsonl(fixture.to_str().unwrap())
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
