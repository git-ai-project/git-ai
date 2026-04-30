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

pub struct ClaudePreset;

impl ClaudePreset {
    fn is_vscode_copilot_hook_payload(data: &serde_json::Value) -> bool {
        if let Some(path) = parse::optional_str(data, "transcript_path") {
            let lower = path.to_lowercase();
            (lower.contains("github copilot") || lower.contains("github.copilot"))
                && !lower.contains(".claude")
        } else {
            data.get("extensionId").is_some()
        }
    }

    fn is_cursor_hook_payload(data: &serde_json::Value) -> bool {
        data.get("cursor_version").is_some()
            || parse::optional_str(data, "transcript_path")
                .map(|p| p.contains(".cursor"))
                .unwrap_or(false)
    }
}

impl AgentPreset for ClaudePreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        if Self::is_vscode_copilot_hook_payload(&data) {
            return Err(GitAiError::PresetError(
                "Skipping VS Code hook payload in Claude preset; use github-copilot hooks."
                    .to_string(),
            ));
        }
        if Self::is_cursor_hook_payload(&data) {
            return Err(GitAiError::PresetError(
                "Skipping Cursor hook payload in Claude preset; use cursor hooks.".to_string(),
            ));
        }

        let cwd = parse::required_str(&data, "cwd")?;
        let transcript_path = parse::required_str(&data, "transcript_path")?;

        let session_id = parse::optional_str(&data, "session_id")
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                parse::required_file_stem(&data, "transcript_path")
                    .unwrap_or_else(|_| "unknown".to_string())
            });

        let tool_name = parse::optional_str_multi(&data, &["tool_name", "toolName"]);
        let hook_event = parse::optional_str_multi(&data, &["hook_event_name", "hookEventName"]);
        let tool_use_id = parse::str_or_default_multi(&data, &["tool_use_id", "toolUseId"], "bash");

        let is_bash = tool_name
            .map(|n| bash_tool::classify_tool(Agent::Claude, n) == ToolClass::Bash)
            .unwrap_or(false);

        let context = PresetContext {
            agent_id: AgentId {
                tool: "claude".to_string(),
                id: session_id.clone(),
                model: "unknown".to_string(),
            },
            session_id: session_id.clone(),
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]),
        };

        let transcript_source = Some(TranscriptSource {
            path: PathBuf::from(transcript_path),
            format: TranscriptFormat::ClaudeJsonl,
            session_id: session_id.clone(),
            external_thread_id: Some(session_id.clone()),
        });

        let event = match (hook_event, is_bash) {
            (Some("PreToolUse"), true) => ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                strategy: BashPreHookStrategy::EmitHumanCheckpoint,
            }),
            (Some("PreToolUse"), false) => ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths: parse::file_paths_from_tool_input(&data, cwd),
                dirty_files: None,
            }),
            (_, true) => ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                transcript_source,
            }),
            (_, false) => ParsedHookEvent::PostFileEdit(PostFileEdit {
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

    fn make_claude_hook_input(event: &str, tool: &str) -> String {
        json!({
            "transcript_path": "/home/user/.claude/projects/abc123.jsonl",
            "cwd": "/home/user/project",
            "hook_event_name": event,
            "tool_name": tool,
            "session_id": "sess-1",
            "tool_use_id": "tu-1",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string()
    }

    #[test]
    fn test_claude_pre_file_edit() {
        let input = make_claude_hook_input("PreToolUse", "Write");
        let events = ClaudePreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "claude");
                assert_eq!(e.context.session_id, "sess-1");
                assert_eq!(e.context.trace_id, "t_test123456789a");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
                assert!(e.dirty_files.is_none());
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_claude_post_file_edit() {
        let input = make_claude_hook_input("PostToolUse", "Write");
        let events = ClaudePreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "claude");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
                assert!(e.transcript_source.is_some());
                if let Some(ts) = &e.transcript_source {
                    assert_eq!(ts.format, TranscriptFormat::ClaudeJsonl);
                    assert_eq!(ts.session_id, "sess-1");
                }
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_claude_pre_bash_call() {
        let input = make_claude_hook_input("PreToolUse", "Bash");
        let events = ClaudePreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "claude");
                assert_eq!(e.tool_use_id, "tu-1");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_claude_post_bash_call() {
        let input = make_claude_hook_input("PostToolUse", "Bash");
        let events = ClaudePreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "claude");
                assert_eq!(e.tool_use_id, "tu-1");
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_claude_session_id_from_filename() {
        let input = json!({
            "transcript_path": "/home/user/.claude/projects/cb947e5b-246e-4253-a953-631f7e464c6b.jsonl",
            "cwd": "/home/user/project",
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string();
        let events = ClaudePreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.session_id, "cb947e5b-246e-4253-a953-631f7e464c6b");
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_claude_skips_vscode_copilot_payload() {
        let input = json!({
            "transcript_path": "/home/user/.vscode/extensions/GitHub Copilot/sessions/test.json",
            "cwd": "/home/user/project",
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string();
        assert!(ClaudePreset.parse(&input, "t_test123456789a").is_err());
    }

    #[test]
    fn test_claude_skips_cursor_payload() {
        let input = json!({
            "transcript_path": "/home/user/.cursor/sessions/test.jsonl",
            "cwd": "/home/user/project",
            "cursor_version": "0.43",
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string();
        assert!(ClaudePreset.parse(&input, "t_test123456789a").is_err());
    }
}
