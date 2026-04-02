use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::transcript::Message;
use git_ai::authorship::working_log::CheckpointKind;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, KimiCodePreset,
};
use serde_json::json;
use std::fs;

// ============================================================================
// Preset parsing tests
// ============================================================================

#[test]
fn test_kimi_code_preset_ai_checkpoint() {
    let hook_input = json!({
        "session_id": "kimi-session-abc-123",
        "cwd": "/Users/test/projects/my-app",
        "model": "moonshot-v1-128k",
        "hook_event_name": "PostToolUse",
        "tool_input": {
            "file_path": "/Users/test/projects/my-app/src/main.rs"
        }
    })
    .to_string();

    let result = KimiCodePreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("KimiCodePreset should run successfully");

    assert_eq!(
        result.checkpoint_kind,
        CheckpointKind::AiAgent,
        "PostToolUse should produce an AiAgent checkpoint"
    );
    assert_eq!(result.agent_id.tool, "kimi-code");
    assert_eq!(result.agent_id.id, "kimi-session-abc-123");
    assert_eq!(result.agent_id.model, "moonshot-v1-128k");
    assert_eq!(
        result.repo_working_dir.as_deref(),
        Some("/Users/test/projects/my-app")
    );
    assert!(
        result.edited_filepaths.is_some(),
        "AI checkpoint should have edited_filepaths"
    );
    assert_eq!(
        result.edited_filepaths.unwrap(),
        vec!["/Users/test/projects/my-app/src/main.rs"]
    );
    assert!(
        result.will_edit_filepaths.is_none(),
        "AI checkpoint should not have will_edit_filepaths"
    );
}

#[test]
fn test_kimi_code_preset_human_checkpoint() {
    let hook_input = json!({
        "session_id": "kimi-session-abc-123",
        "cwd": "/Users/test/projects/my-app",
        "model": "moonshot-v1-128k",
        "hook_event_name": "PreToolUse",
        "tool_input": {
            "file_path": "/Users/test/projects/my-app/src/main.rs"
        }
    })
    .to_string();

    let result = KimiCodePreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("KimiCodePreset should run successfully");

    assert_eq!(
        result.checkpoint_kind,
        CheckpointKind::Human,
        "PreToolUse should produce a Human checkpoint"
    );
    assert_eq!(result.agent_id.tool, "kimi-code");
    assert!(
        result.will_edit_filepaths.is_some(),
        "Human checkpoint should have will_edit_filepaths"
    );
    assert_eq!(
        result.will_edit_filepaths.unwrap(),
        vec!["/Users/test/projects/my-app/src/main.rs"]
    );
    assert!(
        result.edited_filepaths.is_none(),
        "Human checkpoint should not have edited_filepaths"
    );
    assert!(
        result.transcript.is_none(),
        "Human checkpoint should not have transcript"
    );
}

#[test]
fn test_kimi_code_preset_extracts_model() {
    let hook_input = json!({
        "session_id": "session-1",
        "cwd": "/tmp/test",
        "model": "kimi-k2-0711"
    })
    .to_string();

    let result = KimiCodePreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should run");

    assert_eq!(result.agent_id.model, "kimi-k2-0711");
}

#[test]
fn test_kimi_code_preset_defaults_model_to_unknown() {
    let hook_input = json!({
        "session_id": "session-1",
        "cwd": "/tmp/test"
    })
    .to_string();

    let result = KimiCodePreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should run");

    assert_eq!(
        result.agent_id.model, "unknown",
        "Model should default to 'unknown' when not provided"
    );
}

#[test]
fn test_kimi_code_preset_no_filepath_when_tool_input_missing() {
    let hook_input = json!({
        "session_id": "session-1",
        "cwd": "/tmp/test",
        "model": "moonshot-v1"
    })
    .to_string();

    let result = KimiCodePreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should run");

    assert!(
        result.edited_filepaths.is_none(),
        "edited_filepaths should be None when tool_input is missing"
    );
}

#[test]
fn test_kimi_code_preset_with_inline_transcript() {
    let hook_input = json!({
        "session_id": "session-with-transcript",
        "cwd": "/tmp/test",
        "model": "moonshot-v1-128k",
        "transcript": {
            "messages": [
                {
                    "type": "user",
                    "text": "Add a hello world function",
                    "timestamp": "2026-03-15T10:00:00Z"
                },
                {
                    "type": "assistant",
                    "text": "I'll add a hello world function to main.rs.",
                    "timestamp": "2026-03-15T10:00:05Z"
                },
                {
                    "type": "tool_use",
                    "name": "edit",
                    "input": {"file_path": "src/main.rs", "content": "fn hello() {}"},
                    "timestamp": "2026-03-15T10:00:06Z"
                }
            ]
        }
    })
    .to_string();

    let result = KimiCodePreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should run");

    assert!(
        result.transcript.is_some(),
        "Should parse inline transcript"
    );
    let transcript = result.transcript.unwrap();
    assert_eq!(transcript.messages().len(), 3);

    let has_user = transcript
        .messages()
        .iter()
        .any(|m| matches!(m, Message::User { .. }));
    let has_assistant = transcript
        .messages()
        .iter()
        .any(|m| matches!(m, Message::Assistant { .. }));
    let has_tool_use = transcript
        .messages()
        .iter()
        .any(|m| matches!(m, Message::ToolUse { .. }));

    assert!(has_user, "Should have user message");
    assert!(has_assistant, "Should have assistant message");
    assert!(has_tool_use, "Should have tool_use message");
}

#[test]
fn test_kimi_code_preset_no_transcript_when_absent() {
    let hook_input = json!({
        "session_id": "session-no-transcript",
        "cwd": "/tmp/test",
        "model": "moonshot-v1"
    })
    .to_string();

    let result = KimiCodePreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should run");

    assert!(
        result.transcript.is_none(),
        "Transcript should be None when not provided"
    );
}

// ============================================================================
// Error handling tests
// ============================================================================

#[test]
fn test_kimi_code_preset_missing_hook_input() {
    let result = KimiCodePreset.run(AgentCheckpointFlags { hook_input: None });

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("hook_input is required")
    );
}

#[test]
fn test_kimi_code_preset_invalid_json() {
    let result = KimiCodePreset.run(AgentCheckpointFlags {
        hook_input: Some("{ invalid json }".to_string()),
    });

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid JSON"));
}

#[test]
fn test_kimi_code_preset_missing_session_id() {
    let hook_input = json!({
        "cwd": "/tmp/test",
        "model": "moonshot-v1"
    })
    .to_string();

    let result = KimiCodePreset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("session_id not found")
    );
}

#[test]
fn test_kimi_code_preset_missing_cwd() {
    let hook_input = json!({
        "session_id": "session-1",
        "model": "moonshot-v1"
    })
    .to_string();

    let result = KimiCodePreset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("cwd not found"));
}

// ============================================================================
// End-to-end tests using TestRepo
// ============================================================================

#[test]
fn test_kimi_code_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();

    // Create initial file and commit
    let src_dir = repo.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = repo.path().join("src/main.rs");
    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Simulate Kimi Code making an edit
    fs::write(
        &file_path,
        "fn main() {}\n\nfn hello() {\n    println!(\"hello world\");\n}\n",
    )
    .unwrap();

    // Run checkpoint
    let canonical_file = repo.canonical_path().join("src/main.rs");
    let hook_input = json!({
        "session_id": "kimi-e2e-session-001",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "model": "moonshot-v1-128k",
        "hook_event_name": "PostToolUse",
        "tool_input": {
            "file_path": canonical_file.to_string_lossy().to_string()
        }
    })
    .to_string();

    let result = repo
        .git_ai(&["checkpoint", "kimi-code", "--hook-input", &hook_input])
        .expect("checkpoint should succeed");

    // Commit the changes
    let commit = repo
        .stage_all_and_commit("Add hello function")
        .expect("commit should succeed");

    // Verify attribution
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Should have at least one prompt record"
    );

    let prompt = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Prompt record should exist");

    assert_eq!(
        prompt.agent_id.tool, "kimi-code",
        "Should be attributed to kimi-code"
    );
    assert_eq!(
        prompt.agent_id.model, "moonshot-v1-128k",
        "Model should match"
    );
    assert_eq!(
        prompt.agent_id.id, "kimi-e2e-session-001",
        "Session ID should match"
    );
}

#[test]
fn test_kimi_code_e2e_human_then_ai_checkpoint() {
    let repo = TestRepo::new();

    // Create initial file
    let file_path = repo.path().join("index.ts");
    fs::write(&file_path, "console.log('hello');\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Human checkpoint (PreToolUse)
    let pre_hook_input = json!({
        "session_id": "kimi-e2e-session-002",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "model": "moonshot-v1-128k",
        "hook_event_name": "PreToolUse",
        "tool_input": {
            "file_path": "index.ts"
        }
    })
    .to_string();

    repo.git_ai(&["checkpoint", "kimi-code", "--hook-input", &pre_hook_input])
        .expect("human checkpoint should succeed");

    // Make AI edit
    fs::write(
        &file_path,
        "console.log('hello');\nconsole.log('from kimi');\n",
    )
    .unwrap();

    // AI checkpoint (PostToolUse)
    let post_hook_input = json!({
        "session_id": "kimi-e2e-session-002",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "model": "moonshot-v1-128k",
        "hook_event_name": "PostToolUse",
        "tool_input": {
            "file_path": "index.ts"
        }
    })
    .to_string();

    repo.git_ai(&["checkpoint", "kimi-code", "--hook-input", &post_hook_input])
        .expect("AI checkpoint should succeed");

    // Commit
    let commit = repo
        .stage_all_and_commit("Add kimi edit")
        .expect("commit should succeed");

    // Verify attribution — first line human, second line AI
    let mut file = repo.filename("index.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('hello');".human(),
        "console.log('from kimi');".ai(),
    ]);

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Should have attestations"
    );
}

#[test]
fn test_kimi_code_e2e_with_transcript() {
    let repo = TestRepo::new();

    // Create initial file
    let file_path = repo.path().join("app.py");
    fs::write(&file_path, "# app\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make AI edit
    fs::write(&file_path, "# app\ndef greet():\n    print('hi')\n").unwrap();

    // Run checkpoint with inline transcript
    let hook_input = json!({
        "session_id": "kimi-transcript-session",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "model": "moonshot-v1-128k",
        "tool_input": {
            "file_path": "app.py"
        },
        "transcript": {
            "messages": [
                {
                    "type": "user",
                    "text": "Add a greet function"
                },
                {
                    "type": "assistant",
                    "text": "I'll add a greet function to app.py."
                }
            ]
        }
    })
    .to_string();

    repo.git_ai(&["checkpoint", "kimi-code", "--hook-input", &hook_input])
        .expect("checkpoint should succeed");

    let commit = repo
        .stage_all_and_commit("Add greet function")
        .expect("commit should succeed");

    let prompt = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Prompt record should exist");

    assert_eq!(prompt.agent_id.tool, "kimi-code");
    assert!(
        !prompt.messages.is_empty(),
        "Prompt should contain transcript messages"
    );
}

reuse_tests_in_worktree!(
    test_kimi_code_preset_ai_checkpoint,
    test_kimi_code_preset_human_checkpoint,
    test_kimi_code_preset_extracts_model,
    test_kimi_code_preset_defaults_model_to_unknown,
    test_kimi_code_preset_no_filepath_when_tool_input_missing,
    test_kimi_code_preset_with_inline_transcript,
    test_kimi_code_preset_no_transcript_when_absent,
    test_kimi_code_preset_missing_hook_input,
    test_kimi_code_preset_invalid_json,
    test_kimi_code_preset_missing_session_id,
    test_kimi_code_preset_missing_cwd,
    test_kimi_code_e2e_checkpoint_and_commit,
    test_kimi_code_e2e_human_then_ai_checkpoint,
    test_kimi_code_e2e_with_transcript,
);
