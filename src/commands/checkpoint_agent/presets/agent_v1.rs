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
        #[serde(default, alias = "toolUseId")]
        tool_use_id: Option<String>,
        will_edit_filepaths: Option<Vec<String>>,
        #[serde(default)]
        dirty_files: Option<HashMap<String, String>>,
    },
    AiAgent {
        repo_working_dir: String,
        #[serde(default, alias = "toolUseId")]
        tool_use_id: Option<String>,
        edited_filepaths: Option<Vec<String>>,
        #[serde(default)]
        dirty_files: Option<HashMap<String, String>>,
        agent_name: String,
        model: String,
        conversation_id: String,
    },
    PreBashCall {
        repo_working_dir: String,
        #[serde(default, alias = "toolUseId")]
        tool_use_id: Option<String>,
        agent_name: String,
        model: String,
        conversation_id: String,
    },
    PostBashCall {
        repo_working_dir: String,
        #[serde(default, alias = "toolUseId")]
        tool_use_id: Option<String>,
        agent_name: String,
        model: String,
        conversation_id: String,
    },
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
                tool_use_id,
            } => {
                let cwd = PathBuf::from(&repo_working_dir);
                let file_paths = will_edit_filepaths
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| super::parse::resolve_absolute(&p, &repo_working_dir))
                    .collect();
                let dirty = dirty_files.map(|df| {
                    df.into_iter()
                        .map(|(k, v)| (super::parse::resolve_absolute(&k, &repo_working_dir), v))
                        .collect()
                });
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
                    tool_use_id,
                })
            }
            AgentV1Payload::AiAgent {
                repo_working_dir,
                edited_filepaths,
                dirty_files,
                agent_name,
                model,
                conversation_id,
                tool_use_id,
            } => {
                let cwd = PathBuf::from(&repo_working_dir);
                let file_paths = edited_filepaths
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| super::parse::resolve_absolute(&p, &repo_working_dir))
                    .collect();
                let dirty = dirty_files.map(|df| {
                    df.into_iter()
                        .map(|(k, v)| (super::parse::resolve_absolute(&k, &repo_working_dir), v))
                        .collect()
                });
                ParsedHookEvent::PostFileEdit(PostFileEdit {
                    context: PresetContext {
                        agent_id: AgentId {
                            tool: agent_name,
                            id: conversation_id.clone(),
                            model,
                        },
                        external_session_id: conversation_id,
                        trace_id: trace_id.to_string(),
                        cwd,
                        metadata: HashMap::new(),
                    },
                    file_paths,
                    dirty_files: dirty,
                    stream_source: None,
                    tool_use_id,
                })
            }
            AgentV1Payload::PreBashCall {
                repo_working_dir,
                tool_use_id,
                agent_name,
                model,
                conversation_id,
            } => {
                let cwd = PathBuf::from(&repo_working_dir);
                ParsedHookEvent::PreBashCall(PreBashCall {
                    context: PresetContext {
                        agent_id: AgentId {
                            tool: agent_name,
                            id: conversation_id.clone(),
                            model,
                        },
                        external_session_id: conversation_id,
                        trace_id: trace_id.to_string(),
                        cwd,
                        metadata: HashMap::new(),
                    },
                    tool_use_id: tool_use_id.unwrap_or_else(|| "bash".to_string()),
                })
            }
            AgentV1Payload::PostBashCall {
                repo_working_dir,
                tool_use_id,
                agent_name,
                model,
                conversation_id,
            } => {
                let cwd = PathBuf::from(&repo_working_dir);
                ParsedHookEvent::PostBashCall(PostBashCall {
                    context: PresetContext {
                        agent_id: AgentId {
                            tool: agent_name,
                            id: conversation_id.clone(),
                            model,
                        },
                        external_session_id: conversation_id,
                        trace_id: trace_id.to_string(),
                        cwd,
                        metadata: HashMap::new(),
                    },
                    tool_use_id: tool_use_id.unwrap_or_else(|| "bash".to_string()),
                    stream_source: None,
                })
            }
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
    fn test_agent_v1_human_with_tool_use_id() {
        let input = json!({
            "type": "human",
            "repo_working_dir": "/home/user/project",
            "will_edit_filepaths": ["/home/user/project/src/main.rs"],
            "tool_use_id": "tu-abc"
        })
        .to_string();
        let events = AgentV1Preset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.tool_use_id, Some("tu-abc".to_string()));
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_agent_v1_ai_agent_with_tool_use_id() {
        let input = json!({
            "type": "ai_agent",
            "repo_working_dir": "/home/user/project",
            "edited_filepaths": ["/home/user/project/src/lib.rs"],
            "agent_name": "my-agent",
            "model": "gpt-4",
            "conversation_id": "conv-123",
            "tool_use_id": "tu-xyz"
        })
        .to_string();
        let events = AgentV1Preset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.tool_use_id, Some("tu-xyz".to_string()));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_agent_v1_invalid_json() {
        let result = AgentV1Preset.parse("not json", "t_test");
        assert!(result.is_err());
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

    #[test]
    fn test_agent_v1_pre_bash_call_type() {
        let input = json!({
            "type": "pre_bash_call",
            "repo_working_dir": "/home/user/project",
            "tool_use_id": "tu-123",
            "agent_name": "my-agent",
            "model": "gpt-4",
            "conversation_id": "conv-456"
        })
        .to_string();
        let events = AgentV1Preset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "my-agent");
                assert_eq!(e.context.agent_id.model, "gpt-4");
                assert_eq!(e.context.external_session_id, "conv-456");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(e.tool_use_id, "tu-123");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_agent_v1_post_bash_call_type() {
        let input = json!({
            "type": "post_bash_call",
            "repo_working_dir": "/home/user/project",
            "tool_use_id": "tu-123",
            "agent_name": "my-agent",
            "model": "gpt-4",
            "conversation_id": "conv-456"
        })
        .to_string();
        let events = AgentV1Preset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "my-agent");
                assert_eq!(e.context.agent_id.model, "gpt-4");
                assert_eq!(e.context.external_session_id, "conv-456");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(e.tool_use_id, "tu-123");
                assert!(e.stream_source.is_none());
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_agent_v1_pre_bash_call_missing_model_is_error() {
        let input = json!({
            "type": "pre_bash_call",
            "repo_working_dir": "/tmp",
            "tool_use_id": "tu-1",
            "agent_name": "my-agent",
            "conversation_id": "s1"
        })
        .to_string();
        let result = AgentV1Preset.parse(&input, "t_test");
        assert!(result.is_err());
    }

    #[test]
    fn test_agent_v1_pre_bash_call_tool_use_id_defaults_to_bash() {
        let input = json!({
            "type": "pre_bash_call",
            "repo_working_dir": "/tmp",
            "agent_name": "my-agent",
            "model": "unknown",
            "conversation_id": "s1"
        })
        .to_string();
        let events = AgentV1Preset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.tool_use_id, "bash");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }
}
