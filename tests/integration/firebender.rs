use git_ai::authorship::working_log::CheckpointKind;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, FirebenderPreset,
};
use git_ai::error::GitAiError;
use serde_json::json;

#[test]
fn test_firebender_pre_tool_use_maps_to_human_checkpoint() {
    let hook_input = json!({
        "hook_event_name": "preToolUse",
        "model": "gpt-5",
        "workspace_roots": ["/tmp/workspace"],
        "tool_name": "Write",
        "tool_input": {
            "file_path": "src/main.rs"
        },
        "completion_id": "abc123"
    })
    .to_string();

    let result = FirebenderPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .unwrap();

    assert_eq!(result.agent_id.tool, "firebender");
    assert_eq!(result.agent_id.id, "firebender-abc123");
    assert_eq!(result.agent_id.model, "gpt-5");
    assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
    assert_eq!(result.repo_working_dir.as_deref(), Some("/tmp/workspace"));
    assert_eq!(
        result.will_edit_filepaths,
        Some(vec!["src/main.rs".to_string()])
    );
    assert_eq!(result.edited_filepaths, None);
}

#[test]
fn test_firebender_post_tool_use_maps_to_ai_agent_checkpoint() {
    let hook_input = json!({
        "hook_event_name": "postToolUse",
        "model": "claude-sonnet",
        "repo_working_dir": "/tmp/repo",
        "tool_name": "Edit",
        "tool_input": {
            "file_path": "src/lib.rs"
        },
        "completion_id": "done456"
    })
    .to_string();

    let result = FirebenderPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .unwrap();

    assert_eq!(result.agent_id.tool, "firebender");
    assert_eq!(result.agent_id.id, "firebender-done456");
    assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
    assert_eq!(result.repo_working_dir.as_deref(), Some("/tmp/repo"));
    assert_eq!(
        result.edited_filepaths,
        Some(vec!["src/lib.rs".to_string()])
    );
    assert_eq!(result.will_edit_filepaths, None);
}

#[test]
fn test_firebender_edit_supports_apply_patch_path_payloads() {
    let hook_input = json!({
        "hook_event_name": "preToolUse",
        "model": "gpt-5",
        "repo_working_dir": "/tmp/repo",
        "tool_name": "Edit",
        "tool_input": {
            "path": "src/lib.rs",
            "operation_type": "update_file",
            "diff": "@@ ..."
        }
    })
    .to_string();

    let result = FirebenderPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .unwrap();

    assert_eq!(
        result.will_edit_filepaths,
        Some(vec!["src/lib.rs".to_string()])
    );
}

#[test]
fn test_firebender_edit_supports_raw_apply_patch_payloads() {
    let hook_input = json!({
        "hook_event_name": "postToolUse",
        "model": "gpt-5",
        "repo_working_dir": "/tmp/repo",
        "tool_name": "Edit",
        "tool_input": "*** Begin Patch\n*** Update File: src/old.rs\n*** Move to: src/new.rs\n@@\n-old\n+new\n*** End Patch"
    })
    .to_string();

    let result = FirebenderPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .unwrap();

    assert_eq!(
        result.edited_filepaths,
        Some(vec!["src/old.rs".to_string(), "src/new.rs".to_string()])
    );
}

#[test]
fn test_firebender_edit_normalizes_absolute_patch_paths_to_repo_relative() {
    let hook_input = json!({
        "hook_event_name": "postToolUse",
        "model": "gpt-5",
        "repo_working_dir": "/tmp/repo",
        "tool_name": "Edit",
        "tool_input": "*** Begin Patch\n*** Update File: /tmp/repo/src/old.rs\n*** Move to: /tmp/repo/src/new.rs\n@@\n-old\n+new\n*** End Patch"
    })
    .to_string();

    let result = FirebenderPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .unwrap();

    assert_eq!(
        result.edited_filepaths,
        Some(vec!["src/old.rs".to_string(), "src/new.rs".to_string()])
    );
}

#[test]
fn test_firebender_edit_normalizes_absolute_structured_paths_to_repo_relative() {
    let hook_input = json!({
        "hook_event_name": "preToolUse",
        "model": "gpt-5",
        "repo_working_dir": "/tmp/repo",
        "tool_name": "Edit",
        "tool_input": {
            "path": "/tmp/repo/src/lib.rs",
            "operation_type": "update_file",
            "diff": "@@ ..."
        }
    })
    .to_string();

    let result = FirebenderPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .unwrap();

    assert_eq!(
        result.will_edit_filepaths,
        Some(vec!["src/lib.rs".to_string()])
    );
}

#[test]
fn test_firebender_edit_normalizes_windows_absolute_patch_paths_to_repo_relative() {
    let hook_input = json!({
        "hook_event_name": "postToolUse",
        "model": "gpt-5",
        "repo_working_dir": "C:\\repo",
        "tool_name": "Edit",
        "tool_input": "*** Begin Patch\n*** Update File: C:\\repo\\src\\old.rs\n*** Move to: C:\\repo\\src\\new.rs\n@@\n-old\n+new\n*** End Patch"
    })
    .to_string();

    let result = FirebenderPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .unwrap();

    assert_eq!(
        result.edited_filepaths,
        Some(vec!["src/old.rs".to_string(), "src/new.rs".to_string()])
    );
}

#[test]
fn test_firebender_edit_normalizes_windows_absolute_structured_paths_to_repo_relative() {
    let hook_input = json!({
        "hook_event_name": "preToolUse",
        "model": "gpt-5",
        "repo_working_dir": "C:\\repo",
        "tool_name": "Edit",
        "tool_input": {
            "path": "C:\\repo\\src\\lib.rs",
            "operation_type": "update_file",
            "diff": "@@ ..."
        }
    })
    .to_string();

    let result = FirebenderPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .unwrap();

    assert_eq!(
        result.will_edit_filepaths,
        Some(vec!["src/lib.rs".to_string()])
    );
}

#[test]
fn test_firebender_rejects_unknown_event_name() {
    let hook_input = json!({
        "hook_event_name": "somethingElse",
        "model": "gpt-5",
        "tool_name": "Write"
    })
    .to_string();

    let error = FirebenderPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("Invalid hook_event_name: somethingElse")
    );
}

#[test]
fn test_firebender_rejects_non_edit_tools() {
    let hook_input = json!({
        "hook_event_name": "preToolUse",
        "model": "gpt-5",
        "tool_name": "Read"
    })
    .to_string();

    let error = FirebenderPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("Skipping Firebender hook for non-edit tool_name 'Read'.")
    );
}

#[test]
fn test_firebender_preset_missing_hook_input() {
    let preset = FirebenderPreset;
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
fn test_firebender_preset_invalid_json() {
    let preset = FirebenderPreset;
    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some("{invalid".to_string()),
    });

    assert!(result.is_err());
}

#[test]
fn test_firebender_preset_missing_model() {
    let preset = FirebenderPreset;
    let hook_input = json!({
        "hook_event_name": "preToolUse",
        "tool_name": "Write"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    assert!(result.is_err());
    match result {
        Err(GitAiError::PresetError(msg)) => {
            assert!(
                msg.contains("missing field `model`") || msg.contains("Invalid JSON in hook_input")
            );
        }
        _ => panic!("Expected PresetError for missing model"),
    }
}

#[test]
fn test_firebender_preset_empty_model() {
    let preset = FirebenderPreset;
    let hook_input = json!({
        "hook_event_name": "preToolUse",
        "model": "   ",
        "tool_name": "Write"
    })
    .to_string();

    let result = preset.run(AgentCheckpointFlags {
        hook_input: Some(hook_input),
    });

    let result = result.expect("Empty model should fall back to unknown");
    assert_eq!(result.agent_id.model, "unknown");
}

#[test]
fn test_firebender_preset_falls_back_to_first_workspace_root() {
    let preset = FirebenderPreset;
    let hook_input = json!({
        "hook_event_name": "preToolUse",
        "model": "gpt-5",
        "workspace_roots": ["/tmp/workspace1", "/tmp/workspace2"],
        "tool_name": "Write"
    })
    .to_string();

    let result = preset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Should succeed with workspace root fallback");

    assert_eq!(result.repo_working_dir.as_deref(), Some("/tmp/workspace1"));
}
