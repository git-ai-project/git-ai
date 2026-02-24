#[macro_use]
mod repos;
mod test_utils;

use git_ai::authorship::working_log::CheckpointKind;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, AiTabPreset, ClaudePreset, CodexPreset,
    ContinueCliPreset, CursorPreset, DroidPreset, GeminiPreset, GithubCopilotPreset,
};
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;

// ==============================================================================
// ClaudePreset Error Cases
// ==============================================================================

#[test]
fn test_claude_preset_missing_hook_input() {
    let preset = ClaudePreset;
    let result = preset.run(AgentCheckpointFlags { hook_input: None });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("hook_input is required"));
        }
        _ => panic!("Expected PresetError for missing hook_input"),
    }
}

#[test]
fn test_claude_preset_invalid_json() {
    let preset = ClaudePreset;
    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some("not valid json".to_string()),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("Invalid JSON"));
        }
        _ => panic!("Expected PresetError for invalid JSON"),
    }
}

#[test]
fn test_claude_preset_missing_transcript_path() {
    let preset = ClaudePreset;
    let hook_input = json!({
        "cwd": "/some/path",
        "hook_event_name": "PostToolUse"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("transcript_path not found"));
        }
        _ => panic!("Expected PresetError for missing transcript_path"),
    }
}

#[test]
fn test_claude_preset_missing_cwd() {
    let preset = ClaudePreset;
    let hook_input = json!({
        "transcript_path": "tests/fixtures/example-claude-code.jsonl",
        "hook_event_name": "PostToolUse"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("cwd not found"));
        }
        _ => panic!("Expected PresetError for missing cwd"),
    }
}

#[test]
fn test_claude_preset_pretooluse_checkpoint() {
    let preset = ClaudePreset;
    let hook_input = json!({
        "cwd": "/some/path",
        "hook_event_name": "PreToolUse",
        "transcript_path": "tests/fixtures/example-claude-code.jsonl",
        "tool_input": {
            "file_path": "/some/file.rs"
        }
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed for PreToolUse");

    assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
    assert!(result.transcript.is_none());
    assert!(result.edited_filepaths.is_none());
    assert_eq!(
        result.will_edit_filepaths,
        Some(vec!["/some/file.rs".to_string()])
    );
}

#[test]
fn test_claude_preset_invalid_transcript_path() {
    let preset = ClaudePreset;
    let hook_input = json!({
        "cwd": "/some/path",
        "hook_event_name": "PostToolUse",
        "transcript_path": "/nonexistent/path/to/transcript.jsonl"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    // Should succeed but have empty transcript due to error handling
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(result.transcript.is_some());
    assert_eq!(result.agent_id.model, "unknown");
}

#[test]
fn test_claude_transcript_parsing_empty_file() {
    let temp_file = std::env::temp_dir().join("empty_claude.jsonl");
    fs::write(&temp_file, "").expect("Failed to write temp file");

    let result =
        ClaudePreset::transcript_and_model_from_claude_code_jsonl(temp_file.to_str().unwrap());

    assert!(result.is_ok());
    let (transcript, model) = result.unwrap();
    assert!(transcript.messages().is_empty());
    assert!(model.is_none());

    fs::remove_file(temp_file).ok();
}

#[test]
fn test_claude_transcript_parsing_malformed_json() {
    let temp_file = std::env::temp_dir().join("malformed_claude.jsonl");
    fs::write(&temp_file, "{invalid json}\n").expect("Failed to write temp file");

    let result =
        ClaudePreset::transcript_and_model_from_claude_code_jsonl(temp_file.to_str().unwrap());

    assert!(result.is_err());
    fs::remove_file(temp_file).ok();
}

#[test]
fn test_claude_transcript_parsing_with_empty_lines() {
    let temp_file = std::env::temp_dir().join("empty_lines_claude.jsonl");
    let content = r#"
{"type":"user","timestamp":"2025-01-01T00:00:00Z","message":{"content":"test"}}

{"type":"assistant","timestamp":"2025-01-01T00:00:01Z","message":{"model":"claude-3","content":[{"type":"text","text":"response"}]}}
    "#;
    fs::write(&temp_file, content).expect("Failed to write temp file");

    let result =
        ClaudePreset::transcript_and_model_from_claude_code_jsonl(temp_file.to_str().unwrap());

    assert!(result.is_ok());
    let (transcript, model) = result.unwrap();
    assert_eq!(transcript.messages().len(), 2);
    assert_eq!(model, Some("claude-3".to_string()));

    fs::remove_file(temp_file).ok();
}

#[test]
fn test_claude_vscode_copilot_detection() {
    let preset = ClaudePreset;
    let hook_input = json!({
        "hookEventName": "PostToolUse",
        "toolName": "copilot",
        "sessionId": "test-session",
        "cwd": "/some/path",
        "transcript_path": "/Users/test/Library/Application Support/Code/User/workspaceStorage/workspace-id/GitHub.copilot-chat/transcripts/test-session.jsonl"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("Skipping VS Code hook payload in Claude preset"));
        }
        _ => panic!("Expected PresetError for VS Code Copilot payload in Claude preset"),
    }
}

// ==============================================================================
// GeminiPreset Error Cases
// ==============================================================================

#[test]
fn test_gemini_preset_missing_hook_input() {
    let preset = GeminiPreset;
    let result = preset.run(AgentCheckpointFlags { hook_input: None });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("hook_input is required"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_gemini_preset_invalid_json() {
    let preset = GeminiPreset;
    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some("invalid{json".to_string()),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("Invalid JSON"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_gemini_preset_missing_session_id() {
    let preset = GeminiPreset;
    let hook_input = json!({
        "transcript_path": "tests/fixtures/gemini-session-simple.json",
        "cwd": "/path"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("session_id not found"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_gemini_preset_missing_transcript_path() {
    let preset = GeminiPreset;
    let hook_input = json!({
        "session_id": "test-session",
        "cwd": "/path"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("transcript_path not found"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_gemini_preset_missing_cwd() {
    let preset = GeminiPreset;
    let hook_input = json!({
        "session_id": "test-session",
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("cwd not found"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_gemini_preset_beforetool_checkpoint() {
    let preset = GeminiPreset;
    let hook_input = json!({
        "session_id": "test-session",
        "transcript_path": "tests/fixtures/gemini-session-simple.json",
        "cwd": "/path",
        "hook_event_name": "BeforeTool",
        "tool_input": {
            "file_path": "/file.js"
        }
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed for BeforeTool");

    assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
    assert!(result.transcript.is_none());
    assert_eq!(
        result.will_edit_filepaths,
        Some(vec!["/file.js".to_string()])
    );
}

#[test]
fn test_gemini_transcript_parsing_invalid_path() {
    let result = GeminiPreset::transcript_and_model_from_gemini_json("/nonexistent/path.json");

    assert!(result.is_err());
    match result {
        Err(GitAiError::IoError(_)) => {}
        _ => panic!("Expected IoError"),
    }
}

#[test]
fn test_gemini_transcript_parsing_empty_messages() {
    let temp_file = std::env::temp_dir().join("gemini_empty_messages.json");
    let content = json!({
        "messages": []
    });
    fs::write(&temp_file, content.to_string()).expect("Failed to write temp file");

    let result = GeminiPreset::transcript_and_model_from_gemini_json(temp_file.to_str().unwrap());

    assert!(result.is_ok());
    let (transcript, model) = result.unwrap();
    assert!(transcript.messages().is_empty());
    assert!(model.is_none());

    fs::remove_file(temp_file).ok();
}

#[test]
fn test_gemini_transcript_parsing_missing_messages_field() {
    let temp_file = std::env::temp_dir().join("gemini_no_messages.json");
    let content = json!({
        "other_field": "value"
    });
    fs::write(&temp_file, content.to_string()).expect("Failed to write temp file");

    let result = GeminiPreset::transcript_and_model_from_gemini_json(temp_file.to_str().unwrap());

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("messages array not found"));
        }
        _ => panic!("Expected PresetError"),
    }

    fs::remove_file(temp_file).ok();
}

// ==============================================================================
// ContinueCliPreset Error Cases
// ==============================================================================

#[test]
fn test_continue_preset_missing_hook_input() {
    let preset = ContinueCliPreset;
    let result = preset.run(AgentCheckpointFlags { hook_input: None });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("hook_input is required"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_continue_preset_invalid_json() {
    let preset = ContinueCliPreset;
    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some("not json".to_string()),
    });

    assert!(result.is_err());
}

#[test]
fn test_continue_preset_missing_session_id() {
    let preset = ContinueCliPreset;
    let hook_input = json!({
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json",
        "cwd": "/path",
        "model": "gpt-4"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("session_id not found"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_continue_preset_missing_transcript_path() {
    let preset = ContinueCliPreset;
    let hook_input = json!({
        "session_id": "test-session",
        "cwd": "/path",
        "model": "gpt-4"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("transcript_path not found"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_continue_preset_missing_model_defaults_to_unknown() {
    let preset = ContinueCliPreset;
    let hook_input = json!({
        "session_id": "test-session",
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json",
        "cwd": "/path"
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed with default model");

    // Model should default to "unknown" when not provided
    assert_eq!(result.agent_id.model, "unknown");
}

#[test]
fn test_continue_preset_pretooluse_checkpoint() {
    let preset = ContinueCliPreset;
    let hook_input = json!({
        "session_id": "test-session",
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json",
        "cwd": "/path",
        "model": "gpt-4",
        "hook_event_name": "PreToolUse",
        "tool_input": {
            "file_path": "/file.py"
        }
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed for PreToolUse");

    assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
    assert!(result.transcript.is_none());
    assert_eq!(
        result.will_edit_filepaths,
        Some(vec!["/file.py".to_string()])
    );
}

// ==============================================================================
// CodexPreset Error Cases
// ==============================================================================

#[test]
fn test_codex_preset_missing_hook_input() {
    let preset = CodexPreset;
    let result = preset.run(AgentCheckpointFlags { hook_input: None });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("hook_input is required"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_codex_preset_invalid_json() {
    let preset = CodexPreset;
    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some("{bad json".to_string()),
    });

    assert!(result.is_err());
}

#[test]
fn test_codex_preset_missing_session_id() {
    let preset = CodexPreset;
    let hook_input = json!({
        "type": "agent-turn-complete",
        "transcript_path": "tests/fixtures/codex-session-simple.jsonl",
        "cwd": "/path"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("session_id/thread_id not found"));
        }
        _ => panic!("Expected PresetError for missing session_id/thread_id"),
    }
}

#[test]
fn test_codex_preset_invalid_transcript_path() {
    let preset = CodexPreset;
    let hook_input = json!({
        "type": "agent-turn-complete",
        "session_id": "test-session-12345",
        "transcript_path": "/nonexistent/path/transcript.jsonl",
        "cwd": "/path"
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed with fallback to empty transcript");

    // Should have empty transcript due to error handling
    assert!(result.transcript.is_some());
    // Model defaults to "unknown" when transcript parsing fails
    assert_eq!(result.agent_id.model, "unknown");
    assert_eq!(result.agent_id.id, "test-session-12345");
}

// Note: session_id_from_hook_data is a private function and tested indirectly
// through the public run() method tests above

// ==============================================================================
// CursorPreset Error Cases
// ==============================================================================

#[test]
fn test_cursor_preset_missing_hook_input() {
    let preset = CursorPreset;
    let result = preset.run(AgentCheckpointFlags { hook_input: None });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("hook_input is required"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_cursor_preset_invalid_json() {
    let preset = CursorPreset;
    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some("invalid".to_string()),
    });

    assert!(result.is_err());
}

#[test]
fn test_cursor_preset_missing_conversation_id() {
    let preset = CursorPreset;
    let hook_input = json!({
        "type": "composer_turn_complete",
        "cwd": "/path"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("conversation_id not found"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_cursor_preset_missing_workspace_roots() {
    let preset = CursorPreset;
    let hook_input = json!({
        "type": "composer_turn_complete",
        "conversation_id": "test-conv",
        "hook_event_name": "afterFileEdit"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("workspace_roots not found"));
        }
        _ => panic!("Expected PresetError for missing workspace_roots"),
    }
}

// Note: normalize_cursor_path is a private function and tested indirectly
// through the database operations in the cursor.rs test file

// ==============================================================================
// GithubCopilotPreset Error Cases
// ==============================================================================

#[test]
fn test_github_copilot_preset_missing_hook_input() {
    let preset = GithubCopilotPreset;
    let result = preset.run(AgentCheckpointFlags { hook_input: None });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("hook_input is required"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_github_copilot_preset_invalid_json() {
    let preset = GithubCopilotPreset;
    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some("not json".to_string()),
    });

    assert!(result.is_err());
}

#[test]
fn test_github_copilot_preset_invalid_hook_event_name() {
    let preset = GithubCopilotPreset;
    let hook_input = json!({
        "hook_event_name": "invalid_event_name",
        "sessionId": "test-session",
        "transcriptPath": "tests/fixtures/copilot_session_simple.jsonl"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("Invalid hook_event_name"));
            assert!(msg.contains("before_edit") || msg.contains("after_edit"));
        }
        _ => panic!("Expected PresetError for invalid hook_event_name"),
    }
}

// ==============================================================================
// DroidPreset Error Cases
// ==============================================================================

#[test]
fn test_droid_preset_missing_hook_input() {
    let preset = DroidPreset;
    let result = preset.run(AgentCheckpointFlags { hook_input: None });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("hook_input is required"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_droid_preset_invalid_json() {
    let preset = DroidPreset;
    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some("{invalid".to_string()),
    });

    assert!(result.is_err());
}

#[test]
fn test_droid_preset_generates_fallback_session_id() {
    let preset = DroidPreset;
    let hook_input = json!({
        "transcript_path": "tests/fixtures/droid-session.jsonl",
        "cwd": "/path",
        "hookEventName": "PostToolUse",
        "toolName": "Edit"
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed with generated session_id");

    // Droid generates a fallback session_id if not provided
    assert!(result.agent_id.id.starts_with("droid-"));
    assert_eq!(result.agent_id.tool, "droid");
}

// ==============================================================================
// AiTabPreset Error Cases
// ==============================================================================

#[test]
fn test_aitab_preset_missing_hook_input() {
    let preset = AiTabPreset;
    let result = preset.run(AgentCheckpointFlags { hook_input: None });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("hook_input is required"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_aitab_preset_invalid_json() {
    let preset = AiTabPreset;
    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some("bad json".to_string()),
    });

    assert!(result.is_err());
}

#[test]
fn test_aitab_preset_invalid_hook_event_name() {
    let preset = AiTabPreset;
    let hook_input = json!({
        "hook_event_name": "invalid_event",
        "tool": "test_tool",
        "model": "test_model"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("Unsupported hook_event_name"));
            assert!(msg.contains("expected 'before_edit' or 'after_edit'"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_aitab_preset_empty_tool() {
    let preset = AiTabPreset;
    let hook_input = json!({
        "hook_event_name": "after_edit",
        "tool": "  ",
        "model": "test_model"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("tool must be a non-empty string"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_aitab_preset_empty_model() {
    let preset = AiTabPreset;
    let hook_input = json!({
        "hook_event_name": "after_edit",
        "tool": "test_tool",
        "model": "  "
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(msg.contains("model must be a non-empty string"));
        }
        _ => panic!("Expected PresetError"),
    }
}

#[test]
fn test_aitab_preset_before_edit_checkpoint() {
    let preset = AiTabPreset;
    let hook_input = json!({
        "hook_event_name": "before_edit",
        "tool": "test_tool",
        "model": "gpt-4",
        "repo_working_dir": "/project",
        "will_edit_filepaths": ["/file1.rs", "/file2.rs"]
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed for before_edit");

    assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
    assert!(result.transcript.is_none());
    assert_eq!(result.agent_id.tool, "test_tool");
    assert_eq!(result.agent_id.model, "gpt-4");
    assert_eq!(
        result.will_edit_filepaths,
        Some(vec!["/file1.rs".to_string(), "/file2.rs".to_string()])
    );
}

#[test]
fn test_aitab_preset_after_edit_checkpoint() {
    let preset = AiTabPreset;
    let hook_input = json!({
        "hook_event_name": "after_edit",
        "tool": "test_tool",
        "model": "gpt-4",
        "repo_working_dir": "/project",
        "edited_filepaths": ["/file1.rs"]
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed for after_edit");

    assert_eq!(result.checkpoint_kind, CheckpointKind::AiTab);
    assert!(result.transcript.is_none());
    assert_eq!(result.edited_filepaths, Some(vec!["/file1.rs".to_string()]));
}

#[test]
fn test_aitab_preset_with_dirty_files() {
    let preset = AiTabPreset;
    let mut dirty_files = std::collections::HashMap::new();
    dirty_files.insert("/file1.rs".to_string(), "content1".to_string());
    dirty_files.insert("/file2.rs".to_string(), "content2".to_string());

    let hook_input = json!({
        "hook_event_name": "after_edit",
        "tool": "test_tool",
        "model": "gpt-4",
        "dirty_files": dirty_files
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed with dirty_files");

    assert!(result.dirty_files.is_some());
    let dirty = result.dirty_files.unwrap();
    assert_eq!(dirty.len(), 2);
    assert_eq!(dirty.get("/file1.rs"), Some(&"content1".to_string()));
}

#[test]
fn test_aitab_preset_empty_repo_working_dir_filtered() {
    let preset = AiTabPreset;
    let hook_input = json!({
        "hook_event_name": "after_edit",
        "tool": "test_tool",
        "model": "gpt-4",
        "repo_working_dir": "   "
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed");

    // Empty/whitespace-only repo_working_dir should be filtered to None
    assert!(result.repo_working_dir.is_none());
}

// ==============================================================================
// Integration Tests - Cross-Preset Behavior
// ==============================================================================

#[test]
fn test_all_presets_handle_missing_hook_input_consistently() {
    let presets: Vec<Box<dyn AgentCheckpointPreset>> = vec![
        Box::new(ClaudePreset),
        Box::new(GeminiPreset),
        Box::new(ContinueCliPreset),
        Box::new(CodexPreset),
        Box::new(CursorPreset),
        Box::new(GithubCopilotPreset),
        Box::new(DroidPreset),
        Box::new(AiTabPreset),
    ];

    for preset in presets {
        let result = preset.run(AgentCheckpointFlags { hook_input: None });
        assert!(
            result.is_err(),
            "All presets should fail with missing hook_input"
        );
        match result {
            Err(GitAiError::PresetError(msg)) => {
                assert!(msg.contains("hook_input is required"));
            }
            _ => panic!("Expected PresetError"),
        }
    }
}

#[test]
fn test_all_presets_handle_invalid_json_consistently() {
    let presets: Vec<Box<dyn AgentCheckpointPreset>> = vec![
        Box::new(ClaudePreset),
        Box::new(GeminiPreset),
        Box::new(ContinueCliPreset),
        Box::new(CodexPreset),
        Box::new(CursorPreset),
        Box::new(GithubCopilotPreset),
        Box::new(DroidPreset),
        Box::new(AiTabPreset),
    ];

    for preset in presets {
        let result = preset.run(AgentCheckpointFlags {
            hook_input: Some("{invalid json}".to_string()),
        });
        assert!(result.is_err(), "All presets should fail with invalid JSON");
    }
}

// ==============================================================================
// Edge Cases - Unusual but Valid Inputs
// ==============================================================================

#[test]
fn test_claude_preset_with_tool_input_no_file_path() {
    let preset = ClaudePreset;
    let hook_input = json!({
        "cwd": "/path",
        "hook_event_name": "PostToolUse",
        "transcript_path": "tests/fixtures/example-claude-code.jsonl",
        "tool_input": {
            "other_field": "value"
        }
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed");

    assert!(result.edited_filepaths.is_none());
}

#[test]
fn test_gemini_preset_with_tool_input_no_file_path() {
    let preset = GeminiPreset;
    let hook_input = json!({
        "session_id": "test",
        "transcript_path": "tests/fixtures/gemini-session-simple.json",
        "cwd": "/path",
        "tool_input": {
            "other": "value"
        }
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed");

    assert!(result.edited_filepaths.is_none());
}

#[test]
fn test_continue_preset_with_tool_input_no_file_path() {
    let preset = ContinueCliPreset;
    let hook_input = json!({
        "session_id": "test",
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json",
        "cwd": "/path",
        "model": "gpt-4",
        "tool_input": {}
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed");

    assert!(result.edited_filepaths.is_none());
}

#[test]
fn test_claude_preset_with_unicode_in_path() {
    let preset = ClaudePreset;
    let hook_input = json!({
        "cwd": "/Users/测试/项目",
        "hook_event_name": "PostToolUse",
        "transcript_path": "tests/fixtures/example-claude-code.jsonl",
        "tool_input": {
            "file_path": "/Users/测试/项目/文件.rs"
        }
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should handle unicode paths");

    assert!(result.edited_filepaths.is_some());
    assert_eq!(
        result.edited_filepaths.unwrap()[0],
        "/Users/测试/项目/文件.rs"
    );
}

#[test]
fn test_gemini_transcript_with_unknown_message_types() {
    let temp_file = std::env::temp_dir().join("gemini_unknown_types.json");
    let content = json!({
        "messages": [
            {"type": "user", "content": "test"},
            {"type": "unknown_type", "content": "should be skipped"},
            {"type": "info", "content": "should also be skipped"},
            {"type": "gemini", "content": "response"}
        ]
    });
    fs::write(&temp_file, content.to_string()).expect("Failed to write temp file");

    let result = GeminiPreset::transcript_and_model_from_gemini_json(temp_file.to_str().unwrap())
        .expect("Should parse successfully");

    let (transcript, _) = result;
    // Should only parse user and gemini messages
    assert_eq!(transcript.messages().len(), 2);

    fs::remove_file(temp_file).ok();
}

#[test]
fn test_claude_transcript_with_tool_result_in_user_content() {
    let temp_file = std::env::temp_dir().join("claude_tool_result.jsonl");
    let content = r#"{"type":"user","timestamp":"2025-01-01T00:00:00Z","message":{"content":[{"type":"tool_result","content":"should be skipped"},{"type":"text","text":"actual user input"}]}}
{"type":"assistant","timestamp":"2025-01-01T00:00:01Z","message":{"model":"claude-3","content":[{"type":"text","text":"response"}]}}"#;
    fs::write(&temp_file, content).expect("Failed to write temp file");

    let result =
        ClaudePreset::transcript_and_model_from_claude_code_jsonl(temp_file.to_str().unwrap())
            .expect("Should parse successfully");

    let (transcript, _) = result;
    // Should skip tool_result but include the text content
    let user_messages: Vec<_> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, git_ai::authorship::transcript::Message::User { .. }))
        .collect();
    assert_eq!(user_messages.len(), 1);

    fs::remove_file(temp_file).ok();
}

#[test]
fn test_gemini_transcript_with_empty_tool_calls() {
    let temp_file = std::env::temp_dir().join("gemini_empty_tools.json");
    let content = json!({
        "messages": [
            {
                "type": "gemini",
                "content": "test",
                "toolCalls": []
            }
        ]
    });
    fs::write(&temp_file, content.to_string()).expect("Failed to write temp file");

    let result = GeminiPreset::transcript_and_model_from_gemini_json(temp_file.to_str().unwrap())
        .expect("Should parse successfully");

    let (transcript, _) = result;
    assert_eq!(transcript.messages().len(), 1);

    fs::remove_file(temp_file).ok();
}

#[test]
fn test_gemini_transcript_tool_call_without_args() {
    let temp_file = std::env::temp_dir().join("gemini_tool_no_args.json");
    let content = json!({
        "messages": [
            {
                "type": "gemini",
                "toolCalls": [
                    {"name": "read_file"}
                ]
            }
        ]
    });
    fs::write(&temp_file, content.to_string()).expect("Failed to write temp file");

    let result = GeminiPreset::transcript_and_model_from_gemini_json(temp_file.to_str().unwrap())
        .expect("Should parse successfully");

    let (transcript, _) = result;
    // Tool call should still be added with empty args object
    let tool_uses: Vec<_> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, git_ai::authorship::transcript::Message::ToolUse { .. }))
        .collect();
    assert_eq!(tool_uses.len(), 1);

    fs::remove_file(temp_file).ok();
}
