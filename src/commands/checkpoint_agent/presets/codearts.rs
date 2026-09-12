use super::opencode::OpenCodePreset;
use super::parse;
use super::{
    AgentPreset, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    PresetContext, SessionUpdate, StreamFormat, StreamSource,
};
use crate::authorship::authorship_log_serialization::generate_session_id;
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

fn resolve_stream_source(input: &Value, session_id: &str) -> Option<StreamSource> {
    // The plugin resolves the CodeArts kernel's database path. Do not fall back
    // to OpenCode's default directory or depend on the daemon's environment.
    let path = PathBuf::from(parse::optional_str(input, "transcript_path")?);
    if !path.is_absolute() || !path.is_file() {
        return None;
    }
    let parent_id = OpenCodePreset::lookup_parent_session(&path, session_id);
    Some(StreamSource {
        path,
        format: StreamFormat::OpenCodeSqlite,
        session_id: generate_session_id(session_id, "codearts"),
        external_session_id: session_id.to_string(),
        external_parent_session_id: parent_id,
    })
}

fn preset_context(input: &Value, trace_id: &str, session_id: &str, cwd: &str) -> PresetContext {
    let mut metadata = HashMap::from([("session_id".to_string(), session_id.to_string())]);
    if let Some(tool_name) = parse::optional_str(input, "tool_name") {
        metadata.insert("tool_name".to_string(), tool_name.to_string());
    }
    PresetContext {
        agent_id: AgentId {
            tool: "codearts".to_string(),
            id: session_id.to_string(),
            model: parse::optional_str(input, "model")
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .unwrap_or("unknown")
                .to_string(),
        },
        external_session_id: session_id.to_string(),
        trace_id: trace_id.to_string(),
        cwd: PathBuf::from(cwd),
        metadata,
    }
}

impl AgentPreset for CodeArtsPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let input: Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid CodeArts hook JSON: {e}")))?;
        let is_pre = match parse::required_str(&input, "hook_event_name")? {
            "PreToolUse" => true,
            "PostToolUse" => false,
            "SessionUpdate" => {
                let session_id = required_nonempty(&input, "session_id")?;
                let cwd = required_nonempty(&input, "cwd")?;
                return Ok(resolve_stream_source(&input, session_id)
                    .map(|stream_source| {
                        ParsedHookEvent::SessionUpdate(SessionUpdate {
                            context: preset_context(&input, trace_id, session_id, cwd),
                            stream_source,
                        })
                    })
                    .into_iter()
                    .collect());
            }
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
        let context = preset_context(&input, trace_id, session_id, cwd);

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
                    stream_source: resolve_stream_source(&input, session_id),
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
                    stream_source: resolve_stream_source(&input, session_id),
                    tool_use_id: Some(tool_use_id),
                })
            }
        };
        Ok(vec![event])
    }
}
