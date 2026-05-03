use super::parse;
use super::{
    AgentPreset, BashPreHookStrategy, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall,
    PreFileEdit, PresetContext, TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct ContinueCliPreset;

impl AgentPreset for ContinueCliPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let cwd = parse::required_str(&data, "cwd")?;
        let session_id = parse::required_str(&data, "session_id")?.to_string();
        let transcript_path = parse::required_str(&data, "transcript_path")?;
        let tool_name = parse::optional_str_multi(&data, &["tool_name", "toolName"]);
        let hook_event = parse::optional_str_multi(&data, &["hook_event_name", "hookEventName"]);
        let tool_use_id = parse::str_or_default(&data, "tool_use_id", "bash");

        let is_bash = tool_name
            .map(|n| bash_tool::classify_tool(Agent::ContinueCli, n) == ToolClass::Bash)
            .unwrap_or(false);

        let context = PresetContext {
            agent_id: AgentId {
                tool: "continue-cli".to_string(),
                id: session_id.clone(),
                model: parse::optional_str(&data, "model")
                    .unwrap_or("unknown")
                    .to_string(),
            },
            session_id: session_id.clone(),
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]),
        };

        let transcript_source = Some(TranscriptSource {
            path: PathBuf::from(transcript_path),
            format: TranscriptFormat::ContinueJson,
            session_id: session_id.clone(),
            external_thread_id: Some(session_id.clone()),
        });

        let is_pre = hook_event == Some("PreToolUse");

        let event = match (is_pre, is_bash) {
            (true, true) => ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                strategy: BashPreHookStrategy::EmitHumanCheckpoint,
            }),
            (true, false) => ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths: parse::file_paths_from_tool_input(&data, cwd),
                dirty_files: None,
            }),
            (false, true) => ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                transcript_source,
            }),
            (false, false) => ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths: parse::file_paths_from_tool_input(&data, cwd),
                dirty_files: None,
                transcript_source,
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

    fn make_continue_hook_input(event: &str, tool: &str) -> String {
        json!({
            "transcript_path": "/home/user/.continue/sessions/test.jsonl",
            "cwd": "/home/user/project",
            "hook_event_name": event,
            "tool_name": tool,
            "session_id": "cont-sess-1",
            "tool_use_id": "tu-1",
            "model": "claude-3-sonnet",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string()
    }

    #[test]
    fn test_continue_pre_file_edit() {
        let input = make_continue_hook_input("PreToolUse", "edit");
        let events = ContinueCliPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "continue-cli");
                assert_eq!(e.context.agent_id.model, "claude-3-sonnet");
                assert_eq!(e.context.session_id, "cont-sess-1");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_continue_post_file_edit() {
        let input = make_continue_hook_input("PostToolUse", "edit");
        let events = ContinueCliPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "continue-cli");
                assert!(e.transcript_source.is_some());
                if let Some(ts) = &e.transcript_source {
                    assert_eq!(ts.format, TranscriptFormat::ContinueJson);
                    assert_eq!(ts.session_id, "cont-sess-1");
                }
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_continue_pre_bash_call() {
        let input = make_continue_hook_input("PreToolUse", "terminal");
        let events = ContinueCliPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "continue-cli");
                assert_eq!(e.tool_use_id, "tu-1");
                assert_eq!(e.strategy, BashPreHookStrategy::EmitHumanCheckpoint);
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_continue_post_bash_call() {
        let input = make_continue_hook_input("PostToolUse", "terminal");
        let events = ContinueCliPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "continue-cli");
                assert_eq!(e.tool_use_id, "tu-1");
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_continue_default_model() {
        let input = json!({
            "transcript_path": "/home/user/.continue/sessions/test.jsonl",
            "cwd": "/home/user/project",
            "hook_event_name": "PostToolUse",
            "tool_name": "edit",
            "session_id": "cont-sess-1",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string();
        let events = ContinueCliPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.model, "unknown");
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }
}
