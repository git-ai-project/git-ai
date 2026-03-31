use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::transcript::Message;
use git_ai::authorship::working_log::CheckpointKind;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, WindsurfPreset,
};
use serde_json::json;
use std::fs;
use std::io::Write;

// ============================================================================
// Preset routing tests
// ============================================================================

#[test]
fn test_windsurf_preset_human_checkpoint() {
    let hook_input = json!({
        "trajectory_id": "traj-abc-123",
        "agent_action_name": "pre_write_code",
        "tool_info": {
            "file_path": "/home/user/project/main.rs"
        }
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let result = WindsurfPreset
        .run(flags)
        .expect("Failed to run WindsurfPreset");

    assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
    assert!(result.will_edit_filepaths.is_some());
    assert_eq!(
        result.will_edit_filepaths.unwrap(),
        vec!["/home/user/project/main.rs"]
    );
    assert!(result.edited_filepaths.is_none());
    assert!(result.transcript.is_none());
    assert!(result.agent_metadata.is_none());
    assert_eq!(result.agent_id.tool, "windsurf");
    assert_eq!(result.agent_id.id, "traj-abc-123");
}

#[test]
fn test_windsurf_preset_ai_checkpoint_post_write_code() {
    let hook_input = json!({
        "trajectory_id": "traj-abc-123",
        "agent_action_name": "post_write_code",
        "tool_info": {
            "file_path": "/home/user/project/main.rs"
        }
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let result = WindsurfPreset
        .run(flags)
        .expect("Failed to run WindsurfPreset");

    assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
    assert!(result.edited_filepaths.is_some());
    assert_eq!(
        result.edited_filepaths.unwrap(),
        vec!["/home/user/project/main.rs"]
    );
    assert!(result.will_edit_filepaths.is_none());
    // Transcript parsing will fail since the derived path doesn't exist, but preset handles it gracefully
    assert!(result.transcript.is_some());
    assert!(result.agent_metadata.is_some());
    assert_eq!(result.agent_id.tool, "windsurf");
    assert_eq!(result.agent_id.model, "unknown");
}

#[test]
fn test_windsurf_preset_ai_checkpoint_post_cascade() {
    // Create a temp transcript file using real Windsurf nested format
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        temp_file,
        r#"{{"status":"done","type":"user_input","user_input":{{"user_response":"Hello AI"}}}}"#
    )
    .unwrap();
    writeln!(temp_file, r#"{{"planner_response":{{"response":"I will help you"}},"status":"done","type":"planner_response"}}"#).unwrap();
    let temp_path = temp_file.path().to_str().unwrap().to_string();

    let hook_input = json!({
        "trajectory_id": "traj-abc-123",
        "agent_action_name": "post_cascade_response_with_transcript",
        "tool_info": {
            "transcript_path": temp_path
        }
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let result = WindsurfPreset
        .run(flags)
        .expect("Failed to run WindsurfPreset");

    assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
    assert!(result.transcript.is_some());
    let transcript = result.transcript.unwrap();
    assert_eq!(transcript.messages().len(), 2);

    // Verify message types
    assert!(matches!(&transcript.messages()[0], Message::User { text, .. } if text == "Hello AI"));
    assert!(
        matches!(&transcript.messages()[1], Message::Assistant { text, .. } if text == "I will help you")
    );
}

#[test]
fn test_windsurf_preset_missing_trajectory_id() {
    let hook_input = json!({
        "agent_action_name": "post_write_code"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let result = WindsurfPreset.run(flags);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("trajectory_id not found")
    );
}

#[test]
fn test_windsurf_preset_invalid_json() {
    let flags = AgentCheckpointFlags {
        hook_input: Some("{ invalid json }".to_string()),
    };

    let result = WindsurfPreset.run(flags);
    assert!(result.is_err());
}

// ============================================================================
// Transcript parser tests
// ============================================================================

#[test]
fn test_windsurf_transcript_parser_basic() {
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    writeln!(temp_file, r#"{{"status":"done","type":"user_input","user_input":{{"user_response":"Add a hello world function"}}}}"#).unwrap();
    writeln!(temp_file, r#"{{"planner_response":{{"response":"I'll create a hello world function for you."}},"status":"done","type":"planner_response"}}"#).unwrap();
    writeln!(temp_file, r#"{{"code_action":{{"path":"file:///src/main.rs","new_content":"fn hello() {{ println!(\"Hello!\"); }}"}},"status":"done","type":"code_action"}}"#).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let (transcript, model) = WindsurfPreset::transcript_and_model_from_windsurf_jsonl(temp_path)
        .expect("Failed to parse Windsurf JSONL");

    assert_eq!(transcript.messages().len(), 3);
    // Model is always None for Windsurf
    assert!(model.is_none());

    // Verify user message
    assert!(
        matches!(&transcript.messages()[0], Message::User { text, .. } if text == "Add a hello world function")
    );
    // Verify assistant message
    assert!(
        matches!(&transcript.messages()[1], Message::Assistant { text, .. } if text.contains("hello world"))
    );
    // Verify tool use
    assert!(
        matches!(&transcript.messages()[2], Message::ToolUse { name, .. } if name == "code_action")
    );
}

#[test]
fn test_windsurf_transcript_parser_skips_empty_content() {
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        temp_file,
        r#"{{"status":"done","type":"user_input","user_input":{{"user_response":""}}}}"#
    )
    .unwrap();
    writeln!(temp_file, r#"{{"planner_response":{{"response":"Real response"}},"status":"done","type":"planner_response"}}"#).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let (transcript, _) = WindsurfPreset::transcript_and_model_from_windsurf_jsonl(temp_path)
        .expect("Failed to parse Windsurf JSONL");

    // Empty user_response should be skipped
    assert_eq!(transcript.messages().len(), 1);
    assert!(matches!(
        &transcript.messages()[0],
        Message::Assistant { .. }
    ));
}

#[test]
fn test_windsurf_transcript_parser_handles_malformed_lines() {
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        temp_file,
        r#"{{"status":"done","type":"user_input","user_input":{{"user_response":"Hello"}}}}"#
    )
    .unwrap();
    writeln!(temp_file, "not valid json at all").unwrap();
    writeln!(temp_file, r#"{{"planner_response":{{"response":"Hi there"}},"status":"done","type":"planner_response"}}"#).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let (transcript, _) = WindsurfPreset::transcript_and_model_from_windsurf_jsonl(temp_path)
        .expect("Failed to parse Windsurf JSONL");

    // Malformed line should be skipped
    assert_eq!(transcript.messages().len(), 2);
}

#[test]
fn test_windsurf_transcript_parser_empty_file() {
    let temp_file = tempfile::NamedTempFile::new().unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let (transcript, model) = WindsurfPreset::transcript_and_model_from_windsurf_jsonl(temp_path)
        .expect("Failed to parse empty JSONL");

    assert!(transcript.messages().is_empty());
    assert!(model.is_none());
}

#[test]
fn test_windsurf_transcript_parser_real_fixture() {
    let fixture = crate::test_utils::fixture_path("windsurf-session-simple.jsonl");
    let (transcript, model) =
        WindsurfPreset::transcript_and_model_from_windsurf_jsonl(fixture.to_str().unwrap())
            .expect("Failed to parse real Windsurf JSONL fixture");

    // Model is never present in Windsurf JSONL
    assert!(model.is_none());

    // Should have parsed messages from the real transcript
    assert!(!transcript.messages().is_empty());

    // Check we got user messages
    let user_msgs: Vec<_> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::User { .. }))
        .collect();
    assert!(
        !user_msgs.is_empty(),
        "Should have at least one user message"
    );

    // Check we got assistant messages
    let assistant_msgs: Vec<_> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::Assistant { .. }))
        .collect();
    assert!(
        !assistant_msgs.is_empty(),
        "Should have at least one assistant message"
    );

    // Check we got tool use messages (code_action, view_file, run_command, etc.)
    let tool_msgs: Vec<_> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::ToolUse { .. }))
        .collect();
    assert!(
        !tool_msgs.is_empty(),
        "Should have at least one tool use message"
    );

    // Verify the first user message contains expected content
    if let Message::User { text, .. } = user_msgs[0] {
        assert!(
            text.contains("song"),
            "First user message should mention 'song'"
        );
    }

    // Verify code_action tool uses exist
    let code_actions: Vec<_> = tool_msgs
        .iter()
        .filter(|m| {
            if let Message::ToolUse { name, .. } = m {
                name == "code_action"
            } else {
                false
            }
        })
        .collect();
    assert!(
        !code_actions.is_empty(),
        "Should have code_action tool uses"
    );

    // Print summary for inspection
    println!(
        "Parsed {} total messages from real Windsurf transcript:",
        transcript.messages().len()
    );
    println!("  {} user messages", user_msgs.len());
    println!("  {} assistant messages", assistant_msgs.len());
    println!("  {} tool use messages", tool_msgs.len());
}

#[test]
fn test_windsurf_transcript_maps_all_tool_types() {
    let fixture = crate::test_utils::fixture_path("windsurf-session-simple.jsonl");
    let (transcript, _) =
        WindsurfPreset::transcript_and_model_from_windsurf_jsonl(fixture.to_str().unwrap())
            .expect("Failed to parse Windsurf JSONL");

    let tool_names: Vec<String> = transcript
        .messages()
        .iter()
        .filter_map(|m| {
            if let Message::ToolUse { name, .. } = m {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();

    // The real fixture includes these tool types
    assert!(
        tool_names.contains(&"code_action".to_string()),
        "Should map code_action"
    );
    assert!(
        tool_names.contains(&"view_file".to_string()),
        "Should map view_file"
    );
    assert!(
        tool_names.contains(&"run_command".to_string()),
        "Should map run_command"
    );
    assert!(tool_names.contains(&"find".to_string()), "Should map find");
}

// ============================================================================
// End-to-end tests using TestRepo
// ============================================================================

#[test]
fn test_windsurf_e2e_with_attribution() {
    let repo = TestRepo::new();

    // Create a temp transcript using real Windsurf nested format
    let mut temp_transcript = tempfile::NamedTempFile::new().unwrap();
    writeln!(temp_transcript, r#"{{"status":"done","type":"user_input","user_input":{{"user_response":"add a greeting"}}}}"#).unwrap();
    writeln!(temp_transcript, r#"{{"planner_response":{{"response":"I'll add a greeting line."}},"status":"done","type":"planner_response"}}"#).unwrap();
    writeln!(temp_transcript, r#"{{"code_action":{{"path":"file:///index.ts","new_content":"console.log('hi');"}},"status":"done","type":"code_action"}}"#).unwrap();
    let transcript_path = temp_transcript.path().to_str().unwrap().to_string();

    // Create initial file
    let file_path = repo.path().join("index.ts");
    fs::write(&file_path, "console.log('hello');\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Simulate Windsurf making edits
    fs::write(&file_path, "console.log('hello');\nconsole.log('hi');\n").unwrap();

    // Run checkpoint
    let hook_input = json!({
        "trajectory_id": "traj-001",
        "agent_action_name": "post_write_code",
        "tool_info": {
            "file_path": file_path.to_string_lossy().to_string(),
            "transcript_path": transcript_path
        }
    })
    .to_string();

    repo.git_ai(&["checkpoint", "windsurf", "--hook-input", &hook_input])
        .unwrap();

    // Commit
    let commit = repo.stage_all_and_commit("Add windsurf edit").unwrap();

    // Verify attribution
    let mut file = repo.filename("index.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('hello');".human(),
        "console.log('hi');".ai(),
    ]);

    assert!(!commit.authorship_log.attestations.is_empty());
    assert!(!commit.authorship_log.metadata.prompts.is_empty());

    let prompt_record = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Should have a prompt record");

    assert!(!prompt_record.messages.is_empty());
    assert_eq!(prompt_record.agent_id.tool, "windsurf");
}

#[test]
fn test_windsurf_e2e_human_checkpoint() {
    let repo = TestRepo::new();

    // Create initial file
    let file_path = repo.path().join("index.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Human checkpoint (pre_write_code)
    let hook_input = json!({
        "trajectory_id": "traj-002",
        "agent_action_name": "pre_write_code",
        "tool_info": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    repo.git_ai(&["checkpoint", "windsurf", "--hook-input", &hook_input])
        .unwrap();

    // Make a human edit
    fs::write(&file_path, "const x = 1;\nconst y = 2;\n").unwrap();

    // Commit
    let commit = repo.stage_all_and_commit("Human edit").unwrap();

    // Human edits should be attributed to human
    let mut file = repo.filename("index.ts");
    file.assert_lines_and_blame(crate::lines![
        "const x = 1;".human(),
        "const y = 2;".human(),
    ]);

    assert_eq!(
        commit.authorship_log.attestations.len(),
        0,
        "Human checkpoint should not create AI attestations"
    );
}
