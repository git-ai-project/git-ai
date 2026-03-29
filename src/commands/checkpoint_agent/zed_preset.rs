use crate::{
    authorship::working_log::{AgentId, CheckpointKind},
    commands::checkpoint_agent::agent_presets::{
        AgentCheckpointFlags, AgentCheckpointPreset, AgentRunResult,
    },
    error::GitAiError,
};

pub struct ZedPreset;

impl AgentCheckpointPreset for ZedPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let stdin_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Zed preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&stdin_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let session_id = hook_data
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError("session_id not found in hook_input".to_string())
            })?;

        let cwd = hook_data
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.display().to_string())
            })
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "cwd not found in hook_input and current_dir unavailable".to_string(),
                )
            })?;

        let agent_id = AgentId {
            tool: "zed".to_string(),
            id: session_id.to_string(),
            model: "unknown".to_string(),
        };

        // Extract file_paths array from hook input
        let file_paths: Option<Vec<String>> = hook_data.get("file_paths").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
        });

        let hook_event_name = hook_data
            .get("hook_event_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if hook_event_name == "PreToolUse" {
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: Some(cwd),
                edited_filepaths: None,
                will_edit_filepaths: file_paths,
                dirty_files: None,
            });
        }

        // PostToolUse — AI checkpoint
        Ok(AgentRunResult {
            agent_id,
            agent_metadata: None,
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: None,
            repo_working_dir: Some(cwd),
            edited_filepaths: file_paths,
            will_edit_filepaths: None,
            dirty_files: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zed_preset_pre_tool_use() {
        let input = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "acp-sess-001",
            "cwd": "/tmp/project",
            "file_paths": ["src/main.rs", "src/lib.rs"]
        });

        let preset = ZedPreset;
        let result = preset
            .run(AgentCheckpointFlags {
                hook_input: Some(input.to_string()),
            })
            .unwrap();

        assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
        assert_eq!(result.repo_working_dir, Some("/tmp/project".to_string()));
        assert_eq!(
            result.will_edit_filepaths,
            Some(vec!["src/main.rs".to_string(), "src/lib.rs".to_string()])
        );
        assert!(result.edited_filepaths.is_none());
        assert_eq!(result.agent_id.tool, "zed");
        assert_eq!(result.agent_id.id, "acp-sess-001");
    }

    #[test]
    fn test_zed_preset_post_tool_use() {
        let input = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "acp-sess-001",
            "cwd": "/tmp/project",
            "file_paths": ["src/main.rs"]
        });

        let preset = ZedPreset;
        let result = preset
            .run(AgentCheckpointFlags {
                hook_input: Some(input.to_string()),
            })
            .unwrap();

        assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
        assert_eq!(result.repo_working_dir, Some("/tmp/project".to_string()));
        assert_eq!(
            result.edited_filepaths,
            Some(vec!["src/main.rs".to_string()])
        );
        assert!(result.will_edit_filepaths.is_none());
    }

    #[test]
    fn test_zed_preset_missing_session_id() {
        let input = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "cwd": "/tmp/project"
        });

        let preset = ZedPreset;
        let result = preset.run(AgentCheckpointFlags {
            hook_input: Some(input.to_string()),
        });

        assert!(result.is_err());
    }

    #[test]
    fn test_zed_preset_no_file_paths() {
        let input = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "acp-sess-002",
            "cwd": "/tmp/project"
        });

        let preset = ZedPreset;
        let result = preset
            .run(AgentCheckpointFlags {
                hook_input: Some(input.to_string()),
            })
            .unwrap();

        assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
        assert!(result.edited_filepaths.is_none());
    }

    #[test]
    fn test_zed_preset_missing_hook_input() {
        let preset = ZedPreset;
        let result = preset.run(AgentCheckpointFlags { hook_input: None });
        assert!(result.is_err());
    }
}
