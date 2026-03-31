use git_ai::authorship::working_log::CheckpointKind;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset,
};
use git_ai::commands::checkpoint_agent::agent_v1_preset::AgentV1Preset;
use serde_json::json;

#[test]
fn test_agent_v1_human_checkpoint_with_dirty_files() {
    let hook_input = json!({
        "type": "human",
        "repo_working_dir": "/Users/test/project",
        "will_edit_filepaths": ["/Users/test/project/file.ts"],
        "dirty_files": {
            "/Users/test/project/file.ts": "console.log('hello');"
        }
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = AgentV1Preset;
    let result = preset.run(flags);

    assert!(result.is_ok());
    let run_result = result.unwrap();

    // Assert checkpoint_kind is Human
    assert_eq!(run_result.checkpoint_kind, CheckpointKind::Human);

    // Assert dirty_files is Some with correct content
    assert!(run_result.dirty_files.is_some());
    let dirty_files = run_result.dirty_files.unwrap();
    assert_eq!(dirty_files.len(), 1);
    assert_eq!(
        dirty_files.get("/Users/test/project/file.ts").unwrap(),
        "console.log('hello');"
    );

    // Verify will_edit_filepaths is set
    assert!(run_result.will_edit_filepaths.is_some());
    assert_eq!(
        run_result.will_edit_filepaths.unwrap(),
        vec!["/Users/test/project/file.ts"]
    );

    // Verify agent_id for human checkpoint
    assert_eq!(run_result.agent_id.tool, "human");
    assert_eq!(run_result.agent_id.id, "human");
    assert_eq!(run_result.agent_id.model, "human");
}

#[test]
fn test_agent_v1_ai_agent_checkpoint_with_dirty_files() {
    let hook_input = json!({
        "type": "ai_agent",
        "repo_working_dir": "/Users/test/project",
        "edited_filepaths": ["/Users/test/project/file.ts"],
        "transcript": {"messages": []},
        "agent_name": "test-agent",
        "model": "test-model",
        "conversation_id": "test-123",
        "dirty_files": {
            "/Users/test/project/file.ts": "console.log('hello');"
        }
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = AgentV1Preset;
    let result = preset.run(flags);

    assert!(result.is_ok());
    let run_result = result.unwrap();

    // Assert checkpoint_kind is AiAgent
    assert_eq!(run_result.checkpoint_kind, CheckpointKind::AiAgent);

    // Assert dirty_files is Some with correct content
    assert!(run_result.dirty_files.is_some());
    let dirty_files = run_result.dirty_files.unwrap();
    assert_eq!(dirty_files.len(), 1);
    assert_eq!(
        dirty_files.get("/Users/test/project/file.ts").unwrap(),
        "console.log('hello');"
    );

    // Verify edited_filepaths is set
    assert!(run_result.edited_filepaths.is_some());
    assert_eq!(
        run_result.edited_filepaths.unwrap(),
        vec!["/Users/test/project/file.ts"]
    );

    // Verify agent_id for AI agent checkpoint
    assert_eq!(run_result.agent_id.tool, "test-agent");
    assert_eq!(run_result.agent_id.id, "test-123");
    assert_eq!(run_result.agent_id.model, "test-model");

    // Verify transcript is present
    assert!(run_result.transcript.is_some());
}

#[test]
fn test_agent_v1_human_checkpoint_without_dirty_files() {
    let hook_input = json!({
        "type": "human",
        "repo_working_dir": "/Users/test/project",
        "will_edit_filepaths": ["/Users/test/project/file.ts"]
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = AgentV1Preset;
    let result = preset.run(flags);

    assert!(result.is_ok());
    let run_result = result.unwrap();

    // Assert checkpoint_kind is Human
    assert_eq!(run_result.checkpoint_kind, CheckpointKind::Human);

    // Assert dirty_files is None (backward compatibility)
    assert!(run_result.dirty_files.is_none());

    // Verify will_edit_filepaths is set
    assert!(run_result.will_edit_filepaths.is_some());
    assert_eq!(
        run_result.will_edit_filepaths.unwrap(),
        vec!["/Users/test/project/file.ts"]
    );
}

#[test]
fn test_agent_v1_ai_agent_checkpoint_without_dirty_files() {
    let hook_input = json!({
        "type": "ai_agent",
        "repo_working_dir": "/Users/test/project",
        "edited_filepaths": ["/Users/test/project/file.ts"],
        "transcript": {"messages": []},
        "agent_name": "test-agent",
        "model": "test-model",
        "conversation_id": "test-123"
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = AgentV1Preset;
    let result = preset.run(flags);

    assert!(result.is_ok());
    let run_result = result.unwrap();

    // Assert checkpoint_kind is AiAgent
    assert_eq!(run_result.checkpoint_kind, CheckpointKind::AiAgent);

    // Assert dirty_files is None (backward compatibility)
    assert!(run_result.dirty_files.is_none());

    // Verify edited_filepaths is set
    assert!(run_result.edited_filepaths.is_some());
    assert_eq!(
        run_result.edited_filepaths.unwrap(),
        vec!["/Users/test/project/file.ts"]
    );

    // Verify agent_id
    assert_eq!(run_result.agent_id.tool, "test-agent");
    assert_eq!(run_result.agent_id.id, "test-123");
    assert_eq!(run_result.agent_id.model, "test-model");
}

#[test]
fn test_agent_v1_dirty_files_multiple_files() {
    let hook_input = json!({
        "type": "ai_agent",
        "repo_working_dir": "/Users/test/project",
        "edited_filepaths": ["/Users/test/project/file1.ts", "/Users/test/project/file2.ts"],
        "transcript": {"messages": []},
        "agent_name": "test-agent",
        "model": "test-model",
        "conversation_id": "test-123",
        "dirty_files": {
            "/Users/test/project/file1.ts": "content1",
            "/Users/test/project/file2.ts": "content2"
        }
    });

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = AgentV1Preset;
    let result = preset.run(flags);

    assert!(result.is_ok());
    let run_result = result.unwrap();

    // Assert dirty_files has 2 entries with correct content
    assert!(run_result.dirty_files.is_some());
    let dirty_files = run_result.dirty_files.unwrap();
    assert_eq!(dirty_files.len(), 2);
    assert_eq!(
        dirty_files.get("/Users/test/project/file1.ts").unwrap(),
        "content1"
    );
    assert_eq!(
        dirty_files.get("/Users/test/project/file2.ts").unwrap(),
        "content2"
    );

    // Verify edited_filepaths has both files
    assert!(run_result.edited_filepaths.is_some());
    let edited = run_result.edited_filepaths.unwrap();
    assert_eq!(edited.len(), 2);
    assert!(edited.contains(&"/Users/test/project/file1.ts".to_string()));
    assert!(edited.contains(&"/Users/test/project/file2.ts".to_string()));
}
