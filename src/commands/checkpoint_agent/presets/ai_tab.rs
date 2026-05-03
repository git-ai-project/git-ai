use super::parse;
use super::{AgentPreset, ParsedHookEvent, PostFileEdit, PreFileEdit, PresetContext};
use crate::authorship::working_log::AgentId;
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct AiTabPreset;

impl AgentPreset for AiTabPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let hook_event = parse::required_str(&data, "hook_event_name")?;

        if hook_event != "before_edit" && hook_event != "after_edit" {
            return Err(GitAiError::PresetError(format!(
                "Unsupported hook_event_name '{}' for ai_tab preset (expected 'before_edit' or 'after_edit')",
                hook_event
            )));
        }

        let tool = parse::required_str(&data, "tool")?.trim().to_string();
        if tool.is_empty() {
            return Err(GitAiError::PresetError(
                "tool must be a non-empty string for ai_tab preset".to_string(),
            ));
        }

        let model = parse::required_str(&data, "model")?.trim().to_string();
        if model.is_empty() {
            return Err(GitAiError::PresetError(
                "model must be a non-empty string for ai_tab preset".to_string(),
            ));
        }

        let cwd = parse::optional_str(&data, "repo_working_dir")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(".");

        let completion_id = parse::optional_str(&data, "completion_id")
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis().to_string())
                    .unwrap_or_else(|_| "0".to_string())
            });
        let session_id = format!("ai_tab-{}", completion_id);

        let context = PresetContext {
            agent_id: AgentId {
                tool: tool.clone(),
                id: session_id.clone(),
                model,
            },
            session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::from([("tool".to_string(), tool)]),
        };

        let event = if hook_event == "before_edit" {
            let file_paths = parse::pathbuf_array(&data, "will_edit_filepaths", cwd);
            ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths,
                dirty_files: None,
            })
        } else {
            let file_paths = parse::pathbuf_array(&data, "edited_filepaths", cwd);
            ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths,
                transcript_source: None,
            })
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
    fn test_ai_tab_before_edit() {
        let input = json!({
            "hook_event_name": "before_edit",
            "tool": "supermaven",
            "model": "supermaven-v1",
            "repo_working_dir": "/home/user/project",
            "completion_id": "comp-123",
            "will_edit_filepaths": ["src/main.rs"],
            "dirty_files": {
                "src/main.rs": "old content"
            }
        })
        .to_string();
        let events = AiTabPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "supermaven");
                assert_eq!(e.context.session_id, "ai_tab-comp-123");
                assert_eq!(e.context.agent_id.model, "supermaven-v1");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
                assert_eq!(
                    e.context.metadata.get("tool").map(String::as_str),
                    Some("supermaven")
                );
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_ai_tab_after_edit() {
        let input = json!({
            "hook_event_name": "after_edit",
            "tool": "copilot",
            "model": "gpt-4",
            "repo_working_dir": "/home/user/project",
            "completion_id": "comp-456",
            "edited_filepaths": ["src/lib.rs"]
        })
        .to_string();
        let events = AiTabPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "copilot");
                assert_eq!(e.context.session_id, "ai_tab-comp-456");
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
    fn test_ai_tab_rejects_invalid_event() {
        let input = json!({
            "hook_event_name": "unknown",
            "tool": "copilot",
            "model": "gpt-4"
        })
        .to_string();
        assert!(AiTabPreset.parse(&input, "t_test").is_err());
    }

    #[test]
    fn test_ai_tab_rejects_empty_tool() {
        let input = json!({
            "hook_event_name": "before_edit",
            "tool": "  ",
            "model": "gpt-4"
        })
        .to_string();
        assert!(AiTabPreset.parse(&input, "t_test").is_err());
    }

    #[test]
    fn test_ai_tab_rejects_empty_model() {
        let input = json!({
            "hook_event_name": "before_edit",
            "tool": "copilot",
            "model": "  "
        })
        .to_string();
        assert!(AiTabPreset.parse(&input, "t_test").is_err());
    }

    #[test]
    fn test_ai_tab_defaults_cwd_to_dot() {
        let input = json!({
            "hook_event_name": "before_edit",
            "tool": "copilot",
            "model": "gpt-4",
            "completion_id": "comp-789"
        })
        .to_string();
        let events = AiTabPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.cwd, PathBuf::from("."));
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_ai_tab_generates_session_id_without_completion_id() {
        let input = json!({
            "hook_event_name": "before_edit",
            "tool": "copilot",
            "model": "gpt-4"
        })
        .to_string();
        let events = AiTabPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert!(e.context.session_id.starts_with("ai_tab-"));
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }
}
