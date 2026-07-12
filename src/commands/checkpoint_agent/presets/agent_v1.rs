use super::{
    AgentPreset, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    PresetContext,
};
use crate::authorship::working_log::AgentId;
use crate::error::GitAiError;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct AgentV1Preset;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AgentV1Payload {
    Human {
        repo_working_dir: String,
        will_edit_filepaths: Option<Vec<String>>,
        #[serde(default)]
        dirty_files: Option<HashMap<String, String>>,
    },
    AiAgent {
        repo_working_dir: String,
        edited_filepaths: Option<Vec<String>>,
        #[serde(default)]
        dirty_files: Option<HashMap<String, String>>,
        agent_name: String,
        model: String,
        conversation_id: String,
    },
    PreShellCommand {
        repo_working_dir: String,
        agent_name: String,
        model: String,
        conversation_id: String,
        tool_use_id: Option<String>,
        #[serde(default)]
        command: Option<String>,
    },
    PostShellCommand {
        repo_working_dir: String,
        agent_name: String,
        model: String,
        conversation_id: String,
        tool_use_id: Option<String>,
        #[serde(default)]
        command: Option<String>,
    },
}

fn resolve_paths(paths: Option<Vec<String>>, repo_working_dir: &str) -> Vec<PathBuf> {
    paths
        .unwrap_or_default()
        .into_iter()
        .map(|p| super::parse::resolve_absolute(&p, repo_working_dir))
        .collect()
}

fn resolve_dirty_files(
    dirty_files: Option<HashMap<String, String>>,
    repo_working_dir: &str,
) -> Option<HashMap<PathBuf, String>> {
    dirty_files.map(|df| {
        df.into_iter()
            .map(|(k, v)| (super::parse::resolve_absolute(&k, repo_working_dir), v))
            .collect()
    })
}

fn agent_context(
    repo_working_dir: &str,
    agent_name: String,
    model: String,
    conversation_id: String,
    trace_id: &str,
    command: Option<String>,
) -> PresetContext {
    let mut metadata = HashMap::new();
    if let Some(command) = command {
        metadata.insert("command".to_string(), command);
    }

    PresetContext {
        agent_id: AgentId {
            tool: agent_name,
            id: conversation_id.clone(),
            model,
        },
        external_session_id: conversation_id,
        trace_id: trace_id.to_string(),
        cwd: PathBuf::from(repo_working_dir),
        metadata,
    }
}

impl AgentPreset for AgentV1Preset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let payload: AgentV1Payload = serde_json::from_str(hook_input).map_err(|e| {
            GitAiError::PresetError(format!(
                "Invalid AgentV1Input JSON. Format is documented here: https://usegitai.com/docs/cli/add-your-agent: \n\n Error: {}",
                e
            ))
        })?;

        let event = match payload {
            AgentV1Payload::Human {
                repo_working_dir,
                will_edit_filepaths,
                dirty_files,
            } => {
                let cwd = PathBuf::from(&repo_working_dir);
                let file_paths = resolve_paths(will_edit_filepaths, &repo_working_dir);
                let dirty = resolve_dirty_files(dirty_files, &repo_working_dir);
                ParsedHookEvent::PreFileEdit(PreFileEdit {
                    context: PresetContext {
                        agent_id: AgentId {
                            tool: "human".to_string(),
                            id: "human".to_string(),
                            model: "human".to_string(),
                        },
                        external_session_id: "human".to_string(),
                        trace_id: trace_id.to_string(),
                        cwd,
                        metadata: HashMap::new(),
                    },
                    file_paths,
                    dirty_files: dirty,
                    tool_use_id: None,
                })
            }
            AgentV1Payload::AiAgent {
                repo_working_dir,
                edited_filepaths,
                dirty_files,
                agent_name,
                model,
                conversation_id,
            } => {
                let file_paths = resolve_paths(edited_filepaths, &repo_working_dir);
                let dirty = resolve_dirty_files(dirty_files, &repo_working_dir);
                ParsedHookEvent::PostFileEdit(PostFileEdit {
                    context: agent_context(
                        &repo_working_dir,
                        agent_name,
                        model,
                        conversation_id,
                        trace_id,
                        None,
                    ),
                    file_paths,
                    dirty_files: dirty,
                    stream_source: None,
                    tool_use_id: None,
                })
            }
            AgentV1Payload::PreShellCommand {
                repo_working_dir,
                agent_name,
                model,
                conversation_id,
                tool_use_id,
                command,
            } => ParsedHookEvent::PreBashCall(PreBashCall {
                context: agent_context(
                    &repo_working_dir,
                    agent_name,
                    model,
                    conversation_id,
                    trace_id,
                    command.clone(),
                ),
                tool_use_id: tool_use_id.unwrap_or_else(|| "shell".to_string()),
                command,
            }),
            AgentV1Payload::PostShellCommand {
                repo_working_dir,
                agent_name,
                model,
                conversation_id,
                tool_use_id,
                command,
            } => ParsedHookEvent::PostBashCall(PostBashCall {
                context: agent_context(
                    &repo_working_dir,
                    agent_name,
                    model,
                    conversation_id,
                    trace_id,
                    command.clone(),
                ),
                tool_use_id: tool_use_id.unwrap_or_else(|| "shell".to_string()),
                command,
                stream_source: None,
            }),
        };

        Ok(vec![event])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::checkpoint_agent::presets::*;
    use serde_json::json;

    #[test]
    fn test_agent_v1_human_type() {
        let input = json!({
            "type": "human",
            "repo_working_dir": "/home/user/project",
            "will_edit_filepaths": ["/home/user/project/src/main.rs"],
            "dirty_files": {
                "/home/user/project/src/main.rs": "old content"
            }
        })
        .to_string();
        let events = AgentV1Preset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "human");
                assert_eq!(e.context.external_session_id, "human");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
                assert!(e.dirty_files.is_some());
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_agent_v1_ai_agent_type() {
        let input = json!({
            "type": "ai_agent",
            "repo_working_dir": "/home/user/project",
            "edited_filepaths": ["/home/user/project/src/lib.rs"],
            "transcript": {"messages": []},
            "agent_name": "my-agent",
            "model": "gpt-4",
            "conversation_id": "conv-123"
        })
        .to_string();
        let events = AgentV1Preset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "my-agent");
                assert_eq!(e.context.agent_id.model, "gpt-4");
                assert_eq!(e.context.external_session_id, "conv-123");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/lib.rs")]
                );
                assert!(e.stream_source.is_none());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_agent_v1_human_no_filepaths() {
        let input = json!({
            "type": "human",
            "repo_working_dir": "/home/user/project"
        })
        .to_string();
        let events = AgentV1Preset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert!(e.file_paths.is_empty());
                assert!(e.dirty_files.is_none());
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_agent_v1_invalid_json() {
        let result = AgentV1Preset.parse("not json", "t_test");
        assert!(result.is_err());
    }

    #[test]
    fn test_agent_v1_pre_shell_command_type() {
        let input = json!({
            "type": "pre_shell_command",
            "repo_working_dir": "/home/user/project",
            "agent_name": "my-agent",
            "model": "gpt-4",
            "conversation_id": "conv-123",
            "tool_use_id": "shell-1",
            "command": "printf 'generated\\n' > output.txt"
        })
        .to_string();
        let events = AgentV1Preset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "my-agent");
                assert_eq!(e.context.agent_id.id, "conv-123");
                assert_eq!(e.context.agent_id.model, "gpt-4");
                assert_eq!(e.context.external_session_id, "conv-123");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(e.tool_use_id, "shell-1");
                assert_eq!(
                    e.context.metadata.get("command").map(String::as_str),
                    Some("printf 'generated\\n' > output.txt")
                );
                assert_eq!(
                    e.command.as_deref(),
                    Some("printf 'generated\\n' > output.txt")
                );
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_agent_v1_post_shell_command_type() {
        let input = json!({
            "type": "post_shell_command",
            "repo_working_dir": "/home/user/project",
            "agent_name": "my-agent",
            "model": "gpt-4",
            "conversation_id": "conv-123",
            "tool_use_id": "shell-1",
            "command": "printf 'generated\\n' > output.txt"
        })
        .to_string();
        let events = AgentV1Preset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "my-agent");
                assert_eq!(e.context.agent_id.id, "conv-123");
                assert_eq!(e.context.agent_id.model, "gpt-4");
                assert_eq!(e.context.external_session_id, "conv-123");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(e.tool_use_id, "shell-1");
                assert_eq!(
                    e.context.metadata.get("command").map(String::as_str),
                    Some("printf 'generated\\n' > output.txt")
                );
                assert_eq!(
                    e.command.as_deref(),
                    Some("printf 'generated\\n' > output.txt")
                );
                assert!(e.stream_source.is_none());
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_agent_v1_shell_command_defaults_tool_use_id() {
        let input = json!({
            "type": "pre_shell_command",
            "repo_working_dir": "/home/user/project",
            "agent_name": "my-agent",
            "model": "gpt-4",
            "conversation_id": "conv-123"
        })
        .to_string();
        let events = AgentV1Preset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.tool_use_id, "shell");
                assert!(e.context.metadata.is_empty());
                assert!(e.command.is_none());
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_agent_v1_unknown_type() {
        let input = json!({
            "type": "unknown",
            "repo_working_dir": "/tmp"
        })
        .to_string();
        let result = AgentV1Preset.parse(&input, "t_test");
        assert!(result.is_err());
    }
}
