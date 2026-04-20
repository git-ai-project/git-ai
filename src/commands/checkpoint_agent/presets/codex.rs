use super::parse;
use super::{
    AgentPreset, BashPreHookStrategy, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall,
    PresetContext, TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct CodexPreset;

impl CodexPreset {
    fn session_id_from_hook_data(data: &serde_json::Value) -> Result<String, GitAiError> {
        // Try session_id, thread_id (underscore), and thread-id (hyphen, used by agent-turn-complete)
        parse::optional_str_multi(data, &["session_id", "thread_id"])
            .or_else(|| data.get("thread-id").and_then(|v| v.as_str()))
            .or_else(|| {
                data.get("hook_event")
                    .and_then(|ev| ev.get("thread_id"))
                    .and_then(|v| v.as_str())
            })
            .map(|s| s.to_string())
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "session_id or thread_id not found in hook_input".to_string(),
                )
            })
    }

    fn resolve_transcript_path(data: &serde_json::Value, session_id: &str) -> Option<String> {
        // 1. Explicit transcript_path in hook input
        if let Some(tp) = parse::optional_str(data, "transcript_path") {
            return Some(tp.to_string());
        }

        // 2. Search for latest rollout file on disk
        use crate::commands::checkpoint_agent::transcript_readers;
        match transcript_readers::find_codex_rollout_path_for_session(session_id) {
            Ok(Some(path)) => Some(path.to_string_lossy().to_string()),
            Ok(None) => None,
            Err(e) => {
                eprintln!("[Warning] Failed to locate Codex rollout for session {session_id}: {e}");
                None
            }
        }
    }
}

impl AgentPreset for CodexPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let cwd = parse::required_str(&data, "cwd")?;
        let session_id = Self::session_id_from_hook_data(&data)?;
        let hook_event = parse::optional_str_multi(&data, &["hook_event_name", "hookEventName"]);
        let tool_name = parse::optional_str_multi(&data, &["tool_name", "toolName"]);
        let tool_use_id =
            parse::optional_str_multi(&data, &["tool_use_id", "toolUseId"]).unwrap_or("bash");

        let is_bash = tool_name
            .map(|n| bash_tool::classify_tool(Agent::Codex, n) == ToolClass::Bash)
            .unwrap_or(false);

        let transcript_path = Self::resolve_transcript_path(&data, &session_id);

        let mut metadata = HashMap::new();
        if let Some(ref tp) = transcript_path {
            metadata.insert("transcript_path".to_string(), tp.clone());
        }

        let context = PresetContext {
            agent_id: AgentId {
                tool: "codex".to_string(),
                id: session_id.clone(),
                model: "unknown".to_string(),
            },
            session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata,
        };

        let transcript_source = transcript_path.map(|tp| TranscriptSource::Path {
            path: PathBuf::from(tp),
            format: TranscriptFormat::CodexJsonl,
            session_id: None,
        });

        let event = match hook_event {
            Some("PreToolUse") => {
                if !is_bash {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Codex PreToolUse for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }
                ParsedHookEvent::PreBashCall(PreBashCall {
                    context,
                    tool_use_id: tool_use_id.to_string(),
                    strategy: BashPreHookStrategy::SnapshotOnly,
                })
            }
            Some("PostToolUse") => {
                if !is_bash {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Codex PostToolUse for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }
                ParsedHookEvent::PostBashCall(PostBashCall {
                    context,
                    tool_use_id: tool_use_id.to_string(),
                    transcript_source,
                })
            }
            // "Stop", None, or "agent-turn-complete" etc. -- general checkpoint with transcript
            _ => ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths: vec![],
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

    #[test]
    fn test_codex_pre_bash_call() {
        let input = json!({
            "cwd": "/home/user/project",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "session_id": "codex-sess-1",
            "tool_use_id": "tu-1",
            "transcript_path": "/home/user/.codex/sessions/test.jsonl"
        })
        .to_string();
        let events = CodexPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "codex");
                assert_eq!(e.context.session_id, "codex-sess-1");
                assert_eq!(e.tool_use_id, "tu-1");
                assert_eq!(e.strategy, BashPreHookStrategy::SnapshotOnly);
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_codex_post_bash_call() {
        let input = json!({
            "cwd": "/home/user/project",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "session_id": "codex-sess-1",
            "tool_use_id": "tu-1",
            "transcript_path": "/home/user/.codex/sessions/test.jsonl"
        })
        .to_string();
        let events = CodexPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "codex");
                assert!(matches!(
                    e.transcript_source,
                    Some(TranscriptSource::Path {
                        format: TranscriptFormat::CodexJsonl,
                        ..
                    })
                ));
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_codex_thread_id_fallback() {
        let input = json!({
            "cwd": "/home/user/project",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "thread_id": "thread-abc",
            "tool_use_id": "tu-1"
        })
        .to_string();
        let events = CodexPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.session_id, "thread-abc");
                assert!(e.transcript_source.is_none());
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_codex_rejects_non_bash_tool() {
        let input = json!({
            "cwd": "/home/user/project",
            "hook_event_name": "PostToolUse",
            "tool_name": "write_file",
            "session_id": "codex-sess-1"
        })
        .to_string();
        let result = CodexPreset.parse(&input, "t_test123456789a");
        assert!(result.is_err());
    }

    #[test]
    fn test_codex_missing_session_and_thread_id() {
        let input = json!({
            "cwd": "/home/user/project",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash"
        })
        .to_string();
        let result = CodexPreset.parse(&input, "t_test123456789a");
        assert!(result.is_err());
    }
}
