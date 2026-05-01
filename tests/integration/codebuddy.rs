use crate::test_utils::fixture_path;
use git_ai::authorship::transcript::Message;
use git_ai::authorship::working_log::CheckpointKind;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset,
};
use git_ai::commands::checkpoint_agent::codebuddy_preset::CodeBuddyPreset;

#[test]
fn test_codebuddy_preset_pretooluse_returns_human_checkpoint() {
    let hook_input = serde_json::json!({
        "session_id": "abc123",
        "transcript_path": "/tmp/fake/index.json",
        "cwd": "/",
        "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
        "tool_input": {"filePath": "/tmp/project/main.py", "new_str": "print('hello')"},
        "model": "glm-5.0-turbo",
        "client": "CodeBuddyIDE",
        "version": "4.7.0"
    });

    let result = CodeBuddyPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input.to_string()),
        })
        .unwrap();

    assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
    assert!(result.transcript.is_none());
    assert_eq!(
        result.will_edit_filepaths,
        Some(vec!["/tmp/project/main.py".to_string()])
    );
    assert!(result.edited_filepaths.is_none());
    assert!(result.repo_working_dir.is_none());
}

#[test]
fn test_codebuddy_preset_posttooluse_returns_ai_checkpoint() {
    let hook_input = serde_json::json!({
        "session_id": "abc123",
        "transcript_path": "/tmp/fake/index.json",
        "cwd": "/",
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": {"filePath": "/tmp/project/main.py"},
        "model": "glm-5.0-turbo",
        "client": "CodeBuddyIDE",
        "version": "4.7.0"
    });

    let result = CodeBuddyPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input.to_string()),
        })
        .unwrap();

    assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
    assert_eq!(
        result.edited_filepaths,
        Some(vec!["/tmp/project/main.py".to_string()])
    );
    assert!(result.will_edit_filepaths.is_none());
    assert!(result.repo_working_dir.is_none());
}

#[test]
fn test_codebuddy_preset_extracts_model_from_hook_input() {
    let hook_input = serde_json::json!({
        "session_id": "sess-42",
        "hook_event_name": "PreToolUse",
        "tool_input": {"filePath": "/tmp/project/app.py"},
        "model": "glm-5.0-turbo",
        "client": "CodeBuddyIDE"
    });

    let result = CodeBuddyPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input.to_string()),
        })
        .unwrap();

    assert_eq!(result.agent_id.tool, "codebuddy");
    assert_eq!(result.agent_id.id, "sess-42");
    assert_eq!(result.agent_id.model, "glm-5.0-turbo");
}

#[test]
fn test_codebuddy_preset_ignores_cwd() {
    let hook_input = serde_json::json!({
        "session_id": "abc",
        "cwd": "/",
        "hook_event_name": "PostToolUse",
        "tool_input": {"filePath": "/tmp/project/file.py"},
        "model": "glm-5.0-turbo"
    });

    let result = CodeBuddyPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input.to_string()),
        })
        .unwrap();

    // cwd: "/" should NOT be used as repo_working_dir
    assert!(result.repo_working_dir.is_none());
}

#[test]
fn test_codebuddy_transcript_parsing() {
    let fixture = fixture_path("codebuddy-session/index.json");
    let transcript =
        CodeBuddyPreset::transcript_from_codebuddy_session(fixture.to_str().unwrap()).unwrap();

    let messages = transcript.messages();
    assert_eq!(messages.len(), 3);

    // First message: user
    match &messages[0] {
        Message::User { text, .. } => {
            assert_eq!(text, "Write a function to add two numbers");
        }
        other => panic!("Expected User message, got {:?}", other),
    }

    // Second message: assistant
    match &messages[1] {
        Message::Assistant { text, .. } => {
            assert!(text.contains("def add(a, b)"));
        }
        other => panic!("Expected Assistant message, got {:?}", other),
    }

    // Third message: user
    match &messages[2] {
        Message::User { text, .. } => {
            assert_eq!(text, "Now write tests for it");
        }
        other => panic!("Expected User message, got {:?}", other),
    }
}

#[test]
fn test_codebuddy_preset_missing_hook_input() {
    let result = CodeBuddyPreset.run(AgentCheckpointFlags { hook_input: None });
    assert!(result.is_err());
}

#[test]
fn test_codebuddy_preset_invalid_json() {
    let result = CodeBuddyPreset.run(AgentCheckpointFlags {
        hook_input: Some("not json".to_string()),
    });
    assert!(result.is_err());
}

#[test]
fn test_codebuddy_preset_missing_session_id() {
    let hook_input = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "model": "glm-5.0-turbo"
    });

    let result = CodeBuddyPreset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    });
    assert!(result.is_err());
}

#[test]
fn test_claude_preset_rejects_codebuddy_payload() {
    use git_ai::commands::checkpoint_agent::agent_presets::ClaudePreset;

    let hook_input = serde_json::json!({
        "session_id": "abc123",
        "transcript_path": "/Users/test/Library/Application Support/CodeBuddyExtension/Data/uuid/CodeBuddyIDE/uuid/history/uuid/abc123/index.json",
        "cwd": "/",
        "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
        "tool_input": {"filePath": "/tmp/project/main.py"},
        "model": "glm-5.0-turbo",
        "client": "CodeBuddyIDE"
    });

    let result = ClaudePreset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    });
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("CodeBuddy") || err_msg.contains("codebuddy"),
        "Error should mention CodeBuddy, got: {}",
        err_msg
    );
}
