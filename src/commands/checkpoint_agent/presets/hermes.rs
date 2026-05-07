//! Hermes (NousResearch hermes-agent) preset.
//!
//! Hermes' shell-hook protocol delivers a single-line UTF-8 JSON payload
//! on stdin per the docs at
//! <https://hermes-agent.nousresearch.com/docs/user-guide/features/hooks>.
//! The wire schema (verified against `agent/shell_hooks.py` upstream):
//!
//! ```json
//! {
//!   "hook_event_name": "pre_tool_call",
//!   "tool_name":       "terminal",
//!   "tool_input":      {"command": "ls"},
//!   "session_id":      "sess_abc123",
//!   "cwd":             "/home/user/project",
//!   "extra":           {...}
//! }
//! ```
//!
//! Tool naming uses snake_case lowercase (NOT Claude's PascalCase):
//!   - `write_file` / `patch` → file edit (field: `tool_input.path`)
//!   - `terminal` → bash (field: `tool_input.command`)
//!   - `read_file` / `search_files` → read-only skip
//!
//! Notably **absent** from the payload (verified against upstream
//! `agent/shell_hooks.py`):
//!   - `transcript_path` — Hermes does not surface a transcript file in
//!     hook payloads. Sessions live under `~/.hermes/` keyed by
//!     `session_id`, but the on-disk schema is not part of the public
//!     hook contract.
//!
//! **Events not handled:** `pre_llm_call`, `post_llm_call`,
//! `pre_api_request`, `post_api_request`, `on_session_start`,
//! `on_session_end`, `on_session_finalize`, `on_session_reset`,
//! `subagent_stop`, `transform_tool_result`. The installer subscribes
//! only to `pre_tool_call` and `post_tool_call` (see
//! `mdm/agents/hermes.rs::HERMES_HOOK_EVENTS`), so other events should
//! not normally reach this preset. We defensively reject any event
//! other than these two with an explicit `PresetError`.
//!
//! Transcript reading: not implemented. The hook payload doesn't
//! include a transcript path, and Hermes' on-disk session storage
//! schema is not publicly documented. `transcript_source` is therefore
//! set to `None`. Hook-based attribution still works without it; a
//! dedicated reader can land in a follow-up PR.

use super::parse;
use super::{
    AgentPreset, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    PresetContext,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct HermesPreset;

impl AgentPreset for HermesPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let session_id = parse::required_str(&data, "session_id")?.to_string();
        let cwd = parse::required_str(&data, "cwd")?;

        let tool_name = parse::optional_str(&data, "tool_name");
        let hook_event = parse::optional_str(&data, "hook_event_name");

        let tool_class = tool_name
            .map(|n| bash_tool::classify_tool(Agent::Hermes, n))
            .unwrap_or(ToolClass::Skip);
        let is_bash = tool_class == ToolClass::Bash;
        let is_file_edit = tool_class == ToolClass::FileEdit;

        // Hermes' shell-hook payload doesn't surface a per-tool-call ID.
        // The `extra` dict carries internal kwargs like `task_id` and
        // `tool_call_id`, so honor those when present without making
        // them required.
        let extra_tool_call_id = data
            .get("extra")
            .and_then(|e| e.get("tool_call_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Hermes doesn't expose the active model in the shell-hook
        // payload (it lives in the LLM-call payload, not the tool-call
        // payload). Default to "unknown" rather than reading agent
        // config files.
        let context = PresetContext {
            agent_id: AgentId {
                tool: "hermes".to_string(),
                id: session_id.clone(),
                model: "unknown".to_string(),
            },
            external_session_id: session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::new(),
        };

        let bash_id = extra_tool_call_id.clone().unwrap_or_else(|| "bash".into());

        let event = match hook_event {
            Some("pre_tool_call") => {
                if is_bash {
                    ParsedHookEvent::PreBashCall(PreBashCall {
                        context,
                        tool_use_id: bash_id,
                    })
                } else if is_file_edit {
                    ParsedHookEvent::PreFileEdit(PreFileEdit {
                        context,
                        file_paths: parse::file_paths_from_tool_input(&data, cwd),
                        dirty_files: None,
                        tool_use_id: extra_tool_call_id,
                    })
                } else {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Hermes pre_tool_call for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }
            }
            Some("post_tool_call") => {
                if is_bash {
                    ParsedHookEvent::PostBashCall(PostBashCall {
                        context,
                        tool_use_id: bash_id,
                        // Transcript reader for Hermes' session storage
                        // is not yet implemented; setting None avoids
                        // feeding an undefined format to the existing
                        // readers.
                        transcript_source: None,
                    })
                } else if is_file_edit {
                    ParsedHookEvent::PostFileEdit(PostFileEdit {
                        context,
                        file_paths: parse::file_paths_from_tool_input(&data, cwd),
                        dirty_files: None,
                        transcript_source: None,
                        tool_use_id: extra_tool_call_id,
                    })
                } else {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Hermes post_tool_call for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }
            }
            _ => {
                return Err(GitAiError::PresetError(format!(
                    "Unsupported Hermes hook_event_name: {}",
                    hook_event.unwrap_or("<missing>")
                )));
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

    fn make_hook_input(event: &str, tool: &str, tool_input: serde_json::Value) -> String {
        json!({
            "hook_event_name": event,
            "tool_name": tool,
            "tool_input": tool_input,
            "session_id": "sess_abc123",
            "cwd": "/home/user/project",
            "extra": {},
        })
        .to_string()
    }

    #[test]
    fn test_hermes_pre_file_edit_write_file() {
        let input = make_hook_input(
            "pre_tool_call",
            "write_file",
            json!({"path": "src/main.rs", "content": "fn main() {}"}),
        );
        let events = HermesPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "hermes");
                assert_eq!(e.context.agent_id.id, "sess_abc123");
                assert_eq!(e.context.agent_id.model, "unknown");
                assert_eq!(e.context.external_session_id, "sess_abc123");
                assert_eq!(e.context.trace_id, "t_test123456789a");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
                assert!(e.dirty_files.is_none());
            }
            _ => panic!("Expected PreFileEdit, got {:?}", &events[0]),
        }
    }

    #[test]
    fn test_hermes_post_file_edit_patch() {
        let input = make_hook_input(
            "post_tool_call",
            "patch",
            json!({"path": "/home/user/project/lib.rs", "diff": "..."}),
        );
        let events = HermesPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "hermes");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/lib.rs")]
                );
                assert!(
                    e.transcript_source.is_none(),
                    "Transcript reader not yet implemented; should be None"
                );
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_hermes_pre_bash_call() {
        let input = make_hook_input("pre_tool_call", "terminal", json!({"command": "ls -la"}));
        let events = HermesPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "hermes");
                // No tool_call_id provided in `extra`, so default to "bash".
                assert_eq!(e.tool_use_id, "bash");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_hermes_post_bash_call_with_extra_tool_call_id() {
        let input = json!({
            "hook_event_name": "post_tool_call",
            "tool_name": "terminal",
            "tool_input": {"command": "ls"},
            "session_id": "sess_xyz",
            "cwd": "/tmp",
            "extra": {"tool_call_id": "tc-789", "task_id": "task-1"},
        })
        .to_string();
        let events = HermesPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "hermes");
                assert_eq!(e.tool_use_id, "tc-789");
                assert!(e.transcript_source.is_none());
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_hermes_unsupported_tool_pre_errors() {
        // read_file and search_files are documented but we don't
        // checkpoint for them.
        let input = make_hook_input("pre_tool_call", "read_file", json!({"path": "a.rs"}));
        let result = HermesPreset.parse(&input, "t_test");
        assert!(result.is_err());
        match result {
            Err(GitAiError::PresetError(msg)) => {
                assert!(
                    msg.contains("pre_tool_call for unsupported tool"),
                    "got: {}",
                    msg
                );
            }
            _ => panic!("Expected PresetError"),
        }
    }

    #[test]
    fn test_hermes_unsupported_tool_post_errors() {
        let input = make_hook_input("post_tool_call", "search_files", json!({"query": "TODO"}));
        let result = HermesPreset.parse(&input, "t_test");
        assert!(result.is_err());
    }

    #[test]
    fn test_hermes_lifecycle_event_errors() {
        for event in [
            "pre_llm_call",
            "post_llm_call",
            "pre_api_request",
            "post_api_request",
            "on_session_start",
            "on_session_end",
            "on_session_finalize",
            "on_session_reset",
            "subagent_stop",
            "transform_tool_result",
        ] {
            let input = json!({
                "hook_event_name": event,
                "tool_name": null,
                "tool_input": null,
                "session_id": "sess_lifecycle",
                "cwd": "/tmp",
                "extra": {},
            })
            .to_string();
            let result = HermesPreset.parse(&input, "t_test");
            assert!(result.is_err(), "expected error for {event}");
            match result {
                Err(GitAiError::PresetError(msg)) => {
                    assert!(
                        msg.contains("Unsupported Hermes hook_event_name"),
                        "got: {} for {}",
                        msg,
                        event
                    );
                }
                _ => panic!("Expected PresetError for {event}"),
            }
        }
    }

    #[test]
    fn test_hermes_missing_session_id_errors() {
        let input = json!({
            "hook_event_name": "post_tool_call",
            "tool_name": "write_file",
            "tool_input": {"path": "x"},
            "cwd": "/tmp",
        })
        .to_string();
        let result = HermesPreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("session_id"));
    }

    #[test]
    fn test_hermes_missing_cwd_errors() {
        let input = json!({
            "hook_event_name": "post_tool_call",
            "tool_name": "write_file",
            "tool_input": {"path": "x"},
            "session_id": "sess_1",
        })
        .to_string();
        let result = HermesPreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cwd"));
    }

    #[test]
    fn test_hermes_invalid_json_errors() {
        let result = HermesPreset.parse("not json", "t_test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid JSON in hook_input")
        );
    }

    #[test]
    fn test_hermes_absolute_path_passes_through() {
        let input = make_hook_input(
            "post_tool_call",
            "write_file",
            json!({"path": "/etc/hosts", "content": ""}),
        );
        let events = HermesPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.file_paths, vec![PathBuf::from("/etc/hosts")]);
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }
}
