use super::{AgentPreset, ParsedHookEvent, PostFileEdit, PreFileEdit, PresetContext};
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
    },
    AiAgent {
        repo_working_dir: String,
        edited_filepaths: Option<Vec<String>>,
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
            } => {
                let cwd = PathBuf::from(&repo_working_dir);
                let file_paths = will_edit_filepaths
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| super::parse::resolve_absolute(&p, &repo_working_dir))
                    .collect();
                ParsedHookEvent::PreFileEdit(PreFileEdit {
                    context: PresetContext {
                        agent_id: AgentId {
                            tool: "human".to_string(),
                            id: "human".to_string(),
                            model: "human".to_string(),
                        },
                        session_id: "human".to_string(),
                        trace_id: trace_id.to_string(),
                        cwd,
                        metadata: HashMap::new(),
                    },
                    file_paths,
                    dirty_files: None,
                })
            }
            AgentV1Payload::AiAgent {
                repo_working_dir,
                edited_filepaths,
                agent_name,
                model,
                conversation_id,
            } => {
                let cwd = PathBuf::from(&repo_working_dir);
                let file_paths = edited_filepaths
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| super::parse::resolve_absolute(&p, &repo_working_dir))
                    .collect();
                ParsedHookEvent::PostFileEdit(PostFileEdit {
                    context: PresetContext {
                        agent_id: AgentId {
                            tool: agent_name,
                            id: conversation_id.clone(),
                            model,
                        },
                        session_id: conversation_id,
                        trace_id: trace_id.to_string(),
                        cwd,
                        metadata: HashMap::new(),
                    },
                    file_paths,
                    transcript_source: None,
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
                assert_eq!(e.context.session_id, "human");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
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
                assert_eq!(e.context.session_id, "conv-123");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/lib.rs")]
                );
                assert!(e.transcript_source.is_none());
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
