use super::parse;
use super::{
    AgentPreset, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    PresetContext, TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct GeminiPreset;

impl AgentPreset for GeminiPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let cwd = parse::required_str(&data, "cwd")?;
        let session_id = parse::required_str(&data, "session_id")?.to_string();
        let transcript_path = parse::required_str(&data, "transcript_path")?;
        let tool_name = parse::optional_str_multi(&data, &["tool_name", "toolName"]);
        let hook_event = parse::optional_str_multi(&data, &["hook_event_name", "hookEventName"]);
        let tool_use_id = parse::str_or_default_multi(&data, &["tool_use_id", "toolUseId"], "bash");

        let is_bash = tool_name
            .map(|n| bash_tool::classify_tool(Agent::Gemini, n) == ToolClass::Bash)
            .unwrap_or(false);

        let context = PresetContext {
            agent_id: AgentId {
                tool: "gemini".to_string(),
                id: session_id.clone(),
                model: crate::transcripts::model_extraction::extract_model(
                    Path::new(transcript_path),
                    crate::transcripts::sweep::TranscriptFormat::GeminiJsonl,
                    None,
                )
                .ok()
                .flatten()
                .unwrap_or_else(|| "unknown".to_string()),
            },
            session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]),
        };

        let transcript_source = Some(TranscriptSource {
            path: PathBuf::from(transcript_path),
            format: TranscriptFormat::GeminiJsonl,
            session_id: context.session_id.clone(),
            external_thread_id: None,
        });

        // Gemini uses "BeforeTool" instead of "PreToolUse"
        let is_pre = matches!(hook_event, Some("BeforeTool") | Some("PreToolUse"));

        let event = match (is_pre, is_bash) {
            (true, true) => ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
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

    fn make_gemini_hook_input(event: &str, tool: &str) -> String {
        json!({
            "transcript_path": "/home/user/.gemini/sessions/test.json",
            "cwd": "/home/user/project",
            "hook_event_name": event,
            "tool_name": tool,
            "session_id": "gemini-sess-1",
            "tool_use_id": "tu-1",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string()
    }

    #[test]
    fn test_gemini_pre_file_edit() {
        let input = make_gemini_hook_input("BeforeTool", "write_file");
        let events = GeminiPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "gemini");
                assert_eq!(e.context.session_id, "gemini-sess-1");
                assert_eq!(e.context.trace_id, "t_test123456789a");
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
    fn test_gemini_post_file_edit() {
        let input = make_gemini_hook_input("PostToolUse", "write_file");
        let events = GeminiPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "gemini");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
                assert!(matches!(
                    e.transcript_source,
                    Some(TranscriptSource {
                        format: TranscriptFormat::GeminiJsonl,
                        ..
                    })
                ));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_gemini_pre_bash_call() {
        let input = make_gemini_hook_input("BeforeTool", "shell");
        let events = GeminiPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "gemini");
                assert_eq!(e.tool_use_id, "tu-1");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_gemini_post_bash_call() {
        let input = make_gemini_hook_input("PostToolUse", "shell");
        let events = GeminiPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "gemini");
                assert_eq!(e.tool_use_id, "tu-1");
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_gemini_also_accepts_pre_tool_use() {
        let input = make_gemini_hook_input("PreToolUse", "write_file");
        let events = GeminiPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ParsedHookEvent::PreFileEdit(_)));
    }
}
