use super::opencode::OpenCodePreset;
use super::parse;
use super::{
    AgentPreset, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    PresetContext,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct CodeArtsPreset;

fn required_nonempty<'a>(input: &'a Value, key: &str) -> Result<&'a str, GitAiError> {
    let value = parse::required_str(input, key)?;
    if value.trim().is_empty() {
        return Err(GitAiError::PresetError(format!("{key} must not be empty")));
    }
    Ok(value)
}

impl AgentPreset for CodeArtsPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let input: Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid CodeArts hook JSON: {e}")))?;
        let is_pre = match parse::required_str(&input, "hook_event_name")? {
            "PreToolUse" => true,
            "PostToolUse" => false,
            _ => return Ok(Vec::new()),
        };
        let tool_name = parse::required_str(&input, "tool_name")?;
        let tool_class = bash_tool::classify_tool(Agent::CodeArts, tool_name);
        if tool_class == ToolClass::Skip {
            return Ok(Vec::new());
        }

        let session_id = required_nonempty(&input, "session_id")?;
        let cwd = required_nonempty(&input, "cwd")?;
        let tool_use_id = required_nonempty(&input, "tool_use_id")?.to_string();
        let context = PresetContext {
            agent_id: AgentId {
                tool: "codearts".to_string(),
                id: session_id.to_string(),
                model: parse::optional_str(&input, "model")
                    .map(str::trim)
                    .filter(|model| !model.is_empty())
                    .unwrap_or("unknown")
                    .to_string(),
            },
            external_session_id: session_id.to_string(),
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::from([
                ("session_id".to_string(), session_id.to_string()),
                ("tool_name".to_string(), tool_name.to_string()),
            ]),
        };

        // CodeArts shares the hook protocol, but its transcript storage is not
        // assumed to be OpenCode's database. Model identity comes from the plugin.
        let event = if tool_class == ToolClass::Bash {
            let command = parse::bash_command_from_hook_input(&input);
            if is_pre {
                ParsedHookEvent::PreBashCall(PreBashCall {
                    context,
                    tool_use_id,
                    command,
                })
            } else {
                ParsedHookEvent::PostBashCall(PostBashCall {
                    context,
                    tool_use_id,
                    command,
                    stream_source: None,
                })
            }
        } else {
            let file_paths =
                OpenCodePreset::extract_filepaths_from_tool_input(input.get("tool_input"), cwd);
            // Empty edit paths would capture unrelated changes across the repo.
            if file_paths.is_empty() {
                return Err(GitAiError::PresetError(
                    "CodeArts file edit has no file paths in tool_input".to_string(),
                ));
            }
            if is_pre {
                ParsedHookEvent::PreFileEdit(PreFileEdit {
                    context,
                    file_paths,
                    dirty_files: None,
                    tool_use_id: Some(tool_use_id),
                })
            } else {
                ParsedHookEvent::PostFileEdit(PostFileEdit {
                    context,
                    file_paths,
                    dirty_files: None,
                    stream_source: None,
                    tool_use_id: Some(tool_use_id),
                })
            }
        };
        Ok(vec![event])
    }
}
