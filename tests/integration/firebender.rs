use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use serde_json::json;
use std::path::PathBuf;

fn parse_firebender(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("firebender")?.parse(hook_input, "t_test")
}

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

    let events = parse_firebender(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "firebender");
            assert_eq!(e.context.agent_id.id, "firebender-abc123");
            assert_eq!(e.context.agent_id.model, "gpt-5");
            assert_eq!(e.context.cwd, PathBuf::from("/tmp/workspace"));
            assert_eq!(e.file_paths.len(), 1);
            assert!(e.file_paths[0].to_string_lossy().contains("src/main.rs"));
        }
        _ => panic!("Expected PreFileEdit"),
    }
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

    let events = parse_firebender(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "firebender");
            assert_eq!(e.context.agent_id.id, "firebender-done456");
            assert_eq!(e.context.cwd, PathBuf::from("/tmp/repo"));
            assert_eq!(e.file_paths.len(), 1);
            assert!(e.file_paths[0].to_string_lossy().contains("src/lib.rs"));
        }
        _ => panic!("Expected PostFileEdit"),
    }
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

    let events = parse_firebender(&hook_input).unwrap();
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.file_paths.len(), 1);
            assert!(e.file_paths[0].to_string_lossy().contains("src/lib.rs"));
        }
        _ => panic!("Expected PreFileEdit"),
    }
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

    let events = parse_firebender(&hook_input).unwrap();
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.file_paths.len(), 2);
            let path_strs: Vec<String> = e
                .file_paths
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            assert!(path_strs.iter().any(|p| p.contains("src/old.rs")));
            assert!(path_strs.iter().any(|p| p.contains("src/new.rs")));
        }
        _ => panic!("Expected PostFileEdit"),
    }
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

    let events = parse_firebender(&hook_input).unwrap();
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            let path_strs: Vec<String> = e
                .file_paths
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            assert!(path_strs.iter().any(|p| p.contains("src/old.rs")));
            assert!(path_strs.iter().any(|p| p.contains("src/new.rs")));
        }
        _ => panic!("Expected PostFileEdit"),
    }
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

    let events = parse_firebender(&hook_input).unwrap();
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.file_paths.len(), 1);
            assert!(e.file_paths[0].to_string_lossy().contains("src/lib.rs"));
        }
        _ => panic!("Expected PreFileEdit"),
    }
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

    let events = parse_firebender(&hook_input).unwrap();
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            let path_strs: Vec<String> = e
                .file_paths
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            assert!(path_strs.iter().any(|p| p.contains("src/old.rs")));
            assert!(path_strs.iter().any(|p| p.contains("src/new.rs")));
        }
        _ => panic!("Expected PostFileEdit"),
    }
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

    let events = parse_firebender(&hook_input).unwrap();
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.file_paths.len(), 1);
            assert!(e.file_paths[0].to_string_lossy().contains("src/lib.rs"));
        }
        _ => panic!("Expected PreFileEdit"),
    }
}

#[test]
fn test_firebender_rejects_unknown_event_name() {
    let hook_input = json!({
        "hook_event_name": "somethingElse",
        "model": "gpt-5",
        "tool_name": "Write"
    })
    .to_string();

    let error = parse_firebender(&hook_input).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Invalid hook_event_name: somethingElse")
    );
}

#[test]
fn test_firebender_preset_missing_hook_input() {
    let result = parse_firebender("");
    assert!(result.is_err());
}

#[test]
fn test_firebender_preset_invalid_json() {
    let result = parse_firebender("{invalid");
    assert!(result.is_err());
}

#[test]
fn test_firebender_preset_missing_model() {
    let hook_input = json!({
        "hook_event_name": "preToolUse",
        "tool_name": "Write"
    })
    .to_string();

    let result = parse_firebender(&hook_input);
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
    let hook_input = json!({
        "hook_event_name": "preToolUse",
        "model": "   ",
        "tool_name": "Write"
    })
    .to_string();

    let events = parse_firebender(&hook_input).expect("Empty model should fall back to unknown");
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.context.agent_id.model, "unknown");
        }
        _ => panic!("Expected PreFileEdit"),
    }
}

#[test]
fn test_firebender_preset_falls_back_to_first_workspace_root() {
    let hook_input = json!({
        "hook_event_name": "preToolUse",
        "model": "gpt-5",
        "workspace_roots": ["/tmp/workspace1", "/tmp/workspace2"],
        "tool_name": "Write"
    })
    .to_string();

    let events =
        parse_firebender(&hook_input).expect("Should succeed with workspace root fallback");

    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.context.cwd, PathBuf::from("/tmp/workspace1"));
        }
        _ => panic!("Expected PreFileEdit"),
    }
}
