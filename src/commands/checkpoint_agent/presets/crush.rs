use super::{
    AgentPreset, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    PresetContext,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct CrushPreset;

/// Hook payload produced by Crush's PreToolUse hook.
///
/// Crush natively emits:
/// `{"event":"PreToolUse","session_id":"...","cwd":"...","tool_name":"write","tool_input":{...}}`
///
/// Crush does not support PostToolUse, so the wrapper script installed by
/// git-ai re-invokes this preset with `hook_event_name: "PostToolUse"` after
/// the tool call completes (delayed background call). `model` is likewise
/// injected by the wrapper script when it can be resolved; as a fallback the
/// preset reads it from Crush's per-project session database.
#[derive(Debug, Deserialize)]
struct CrushHookInput {
    #[serde(default)]
    hook_event_name: Option<String>,
    #[serde(default)]
    event: Option<String>,
    session_id: String,
    cwd: String,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<serde_json::Value>,
    #[serde(default, alias = "toolUseId")]
    tool_use_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

impl CrushPreset {
    fn strip_provider_prefix(model: &str) -> String {
        match model.rsplit_once('/') {
            Some((_, name)) if !name.is_empty() => name.to_string(),
            _ => model.to_string(),
        }
    }

    fn is_bash_tool(tool_name: &str) -> bool {
        bash_tool::classify_tool(Agent::Crush, tool_name) == ToolClass::Bash
    }

    fn validate_tool_name(tool_name: &str) -> Result<(), GitAiError> {
        match bash_tool::classify_tool(Agent::Crush, tool_name) {
            ToolClass::FileEdit | ToolClass::Bash => Ok(()),
            ToolClass::Skip => Err(GitAiError::PresetError(format!(
                "Unsupported Crush tool_name: {tool_name}"
            ))),
        }
    }

    /// Locate Crush's per-project session database by walking up from `cwd`
    /// looking for a `.crush/crush.db` directory (Crush creates one data dir
    /// per project). Overridable via `GIT_AI_CRUSH_DB_PATH` for tests.
    fn find_crush_db(cwd: &str) -> Option<PathBuf> {
        if let Ok(test_path) = std::env::var("GIT_AI_CRUSH_DB_PATH") {
            let path = PathBuf::from(test_path);
            if path.is_file() {
                return Some(path);
            }
        }

        let mut current = PathBuf::from(cwd);
        loop {
            let candidate = current.join(".crush").join("crush.db");
            if candidate.is_file() {
                return Some(candidate);
            }
            if !current.pop() {
                return None;
            }
        }
    }

    /// Read the most recent model recorded for this session from Crush's
    /// session database (`messages` table carries model per message).
    fn lookup_model_from_db(cwd: &str, session_id: &str) -> Option<String> {
        let db_path = Self::find_crush_db(cwd)?;
        let conn = crate::sqlite::open_with_flags_and_memory_limits(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()?;

        let model: Option<String> = conn
            .query_row(
                "SELECT model FROM messages \
                 WHERE session_id = ?1 AND model IS NOT NULL AND model != '' \
                 ORDER BY updated_at DESC LIMIT 1",
                [session_id],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        if model.is_none() {
            // Fall back to the most recent model recorded in the project at all
            // (covers sessions whose current turn has not been flushed yet).
            return conn
                .query_row(
                    "SELECT model FROM messages \
                     WHERE model IS NOT NULL AND model != '' \
                     ORDER BY updated_at DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .ok();
        }

        model
    }

    fn resolve_model(input_model: Option<String>, cwd: &str, session_id: &str) -> String {
        let model = input_model
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .or_else(|| Self::lookup_model_from_db(cwd, session_id))
            .unwrap_or_else(|| "unknown".to_string());

        let stripped = Self::strip_provider_prefix(&model);
        if stripped.is_empty() {
            "unknown".to_string()
        } else {
            stripped
        }
    }
}

impl AgentPreset for CrushPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let hook_input: CrushHookInput = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {e}")))?;

        let CrushHookInput {
            hook_event_name,
            event,
            session_id,
            cwd,
            tool_name,
            tool_input,
            tool_use_id,
            model,
        } = hook_input;

        let event_name = hook_event_name.or(event).unwrap_or_else(|| {
            // Default to pre: a bare Crush payload without an explicit event
            // field can only come from a native PreToolUse hook.
            "PreToolUse".to_string()
        });
        if event_name != "PreToolUse" && event_name != "PostToolUse" {
            return Err(GitAiError::PresetError(format!(
                "Unsupported Crush hook_event_name: {event_name}"
            )));
        }
        let is_pre = event_name == "PreToolUse";

        // Crush always provides a tool_name; treat missing as a bash call so
        // the wrapper script's bash handling keeps working.
        let tool_name = tool_name.unwrap_or_else(|| "bash".to_string());
        Self::validate_tool_name(&tool_name)?;
        let is_bash = Self::is_bash_tool(&tool_name);

        let file_paths =
            crate::commands::checkpoint_agent::presets::OpenCodePreset::extract_filepaths_from_tool_input(
                tool_input.as_ref(),
                &cwd,
            );
        let bash_command = tool_input
            .as_ref()
            .and_then(|value| {
                value
                    .get("command")
                    .or_else(|| value.get("cmd"))
                    .and_then(|v| v.as_str())
            })
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
        let tool_use_id_str = tool_use_id.unwrap_or_else(|| "bash".to_string());

        let model_final = Self::resolve_model(model, &cwd, &session_id);

        let mut metadata = HashMap::new();
        metadata.insert("session_id".to_string(), session_id.clone());
        metadata.insert("tool_name".to_string(), tool_name.clone());

        let context = PresetContext {
            agent_id: AgentId {
                tool: "crush".to_string(),
                id: session_id.clone(),
                model: model_final,
            },
            external_session_id: session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(&cwd),
            metadata,
        };

        let event = match (is_pre, is_bash) {
            (true, true) => ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id: tool_use_id_str,
                command: bash_command,
            }),
            (true, false) => ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths,
                dirty_files: None,
                tool_use_id: Some(tool_use_id_str),
            }),
            (false, true) => ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id: tool_use_id_str,
                command: bash_command,
                stream_source: None,
            }),
            (false, false) => ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths,
                dirty_files: None,
                stream_source: None,
                tool_use_id: Some(tool_use_id_str),
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
    use std::path::PathBuf;

    #[test]
    fn test_crush_pre_file_edit() {
        let input = json!({
            "event": "PreToolUse",
            "session_id": "crush-sess-123",
            "cwd": "/tmp/project",
            "tool_name": "write",
            "tool_input": {"file_path": "/tmp/project/src/main.rs", "content": "fn main() {}"}
        })
        .to_string();
        let events = CrushPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "crush");
                assert_eq!(e.context.external_session_id, "crush-sess-123");
                assert_eq!(e.context.cwd, PathBuf::from("/tmp/project"));
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/tmp/project/src/main.rs")]
                );
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_crush_post_file_edit_with_model() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "session_id": "crush-sess-123",
            "cwd": "/tmp/project",
            "tool_name": "edit",
            "tool_input": {"file_path": "src/main.rs"},
            "model": "glm-53-fp8"
        })
        .to_string();
        let events = CrushPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.model, "glm-53-fp8");
                assert!(!e.file_paths.is_empty());
                assert!(e.stream_source.is_none());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_crush_pre_bash_call() {
        let input = json!({
            "event": "PreToolUse",
            "session_id": "crush-sess-123",
            "cwd": "/tmp/project",
            "tool_name": "bash",
            "tool_input": {"command": "echo hi >> notes.txt"}
        })
        .to_string();
        let events = CrushPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.command.as_deref(), Some("echo hi >> notes.txt"));
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_crush_unknown_model_falls_back_to_unknown() {
        let input = json!({
            "event": "PreToolUse",
            "session_id": "crush-sess-123",
            "cwd": "/definitely/not/a/crush/project",
            "tool_name": "write",
            "tool_input": {"file_path": "/definitely/not/a/crush/project/a.txt"}
        })
        .to_string();
        let events = CrushPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.model, "unknown");
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_crush_unsupported_event_rejected() {
        let input = json!({
            "hook_event_name": "Stop",
            "session_id": "crush-sess-123",
            "cwd": "/tmp/project",
            "tool_name": "write",
            "tool_input": {"file_path": "/tmp/project/a.txt"}
        })
        .to_string();
        assert!(CrushPreset.parse(&input, "t_test").is_err());
    }

    #[test]
    fn test_crush_unsupported_tool_rejected() {
        let input = json!({
            "event": "PreToolUse",
            "session_id": "crush-sess-123",
            "cwd": "/tmp/project",
            "tool_name": "web_search",
            "tool_input": {}
        })
        .to_string();
        assert!(CrushPreset.parse(&input, "t_test").is_err());
    }
}
