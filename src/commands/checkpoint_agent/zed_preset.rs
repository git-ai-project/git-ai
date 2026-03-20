use crate::{
    authorship::working_log::{AgentId, CheckpointKind},
    commands::checkpoint_agent::agent_presets::{
        AgentCheckpointFlags, AgentCheckpointPreset, AgentRunResult,
    },
    error::GitAiError,
};
use serde::Deserialize;

pub struct ZedPreset;

/// Hook input from git-ai MCP server or direct CLI invocation
#[derive(Debug, Deserialize)]
struct ZedHookInput {
    hook_event_name: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    edited_filepaths: Option<Vec<String>>,
    #[serde(default)]
    tool_input: Option<ZedToolInput>,
}

#[derive(Debug, Deserialize)]
struct ZedToolInput {
    #[serde(default)]
    file_paths: Option<Vec<String>>,
}

impl AgentCheckpointPreset for ZedPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Zed preset".to_string())
        })?;

        let hook_input: ZedHookInput = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let is_pre_tool_use = hook_input.hook_event_name == "PreToolUse";

        // Extract file paths from edited_filepaths or tool_input
        let file_paths = hook_input
            .edited_filepaths
            .or_else(|| hook_input.tool_input.and_then(|ti| ti.file_paths))
            .filter(|paths| !paths.is_empty());

        // Use session_id from MCP server (stable per server instance) or generate fallback
        let session_id = hook_input
            .session_id
            .unwrap_or_else(|| "zed-unknown".to_string());

        let agent_id = AgentId {
            tool: "zed".to_string(),
            id: session_id,
            model: "unknown".to_string(),
        };

        if is_pre_tool_use {
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: hook_input.cwd,
                edited_filepaths: None,
                will_edit_filepaths: file_paths,
                dirty_files: None,
            });
        }

        // PostToolUse - AI checkpoint
        Ok(AgentRunResult {
            agent_id,
            agent_metadata: None,
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: None,
            repo_working_dir: hook_input.cwd,
            edited_filepaths: file_paths,
            will_edit_filepaths: None,
            dirty_files: None,
        })
    }
}
