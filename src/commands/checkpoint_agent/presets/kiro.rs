//! Kiro (kiro-cli, AWS Kiro IDE) preset.
//!
//! Kiro CLI's hook protocol delivers a single-line UTF-8 JSON payload
//! on stdin per the docs at <https://kiro.dev/docs/cli/hooks/>. The
//! wire schema (verified against the public docs):
//!
//! ```json
//! {
//!   "hook_event_name": "preToolUse",
//!   "cwd": "/home/user/project",
//!   "session_id": "f2946a26-3735-4b08-8d05-c928010302d5",
//!   "tool_name": "fs_write",
//!   "tool_input": {"path": "src/main.rs", "content": "..."}
//! }
//! ```
//!
//! Notable differences from Claude:
//!   - `hook_event_name` is **camelCase** (`preToolUse`,
//!     `postToolUse`) — Claude uses PascalCase (`PreToolUse`).
//!   - `tool_name` is snake_case (`fs_write`, `execute_bash`,
//!     `use_aws`) — Claude uses PascalCase.
//!   - There is **no `transcript_path` field**. Kiro stores
//!     conversation history in a SQLite database under `~/.kiro/`
//!     keyed by `session_id`, but the schema is not publicly
//!     documented; this preset cannot resolve it without
//!     reverse-engineering.
//!
//! Tool naming (snake_case lowercase):
//!   - `fs_write` (alias `write`)       → file edit (field:
//!     `tool_input.path`)
//!   - `execute_bash` (alias `shell`)   → bash (field:
//!     `tool_input.command`)
//!   - `fs_read` (alias `read`)         → read-only skip
//!   - `use_aws` (alias `aws`)          → external skip
//!   - `@server/tool`                   → MCP-namespaced skip
//!
//! Path resolution: `tool_input.path` may be relative to `cwd` or
//! absolute. Already-absolute paths pass through unchanged; relative
//! paths are resolved against `cwd`.
//!
//! **Events not handled:** `agentSpawn`, `userPromptSubmit`, `stop`.
//! The installer subscribes only to `preToolUse` and `postToolUse`
//! (see `mdm/agents/kiro.rs::KIRO_HOOK_EVENTS`), so other events
//! should not normally reach this preset. We defensively reject any
//! event other than these two with an explicit `PresetError`.
//!
//! Transcript reading: not implemented. Kiro's session storage uses
//! an undocumented SQLite schema under `~/.kiro/`, and the hook
//! payload doesn't include a transcript path. `transcript_source` is
//! therefore set to `None`. Hook-based attribution still works without
//! it; a SQLite-based reader can land in a follow-up once Kiro
//! documents the format (or upstream provides an export hook).

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

pub struct KiroPreset;

impl AgentPreset for KiroPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let session_id = parse::required_str(&data, "session_id")?.to_string();
        let cwd = parse::required_str(&data, "cwd")?;

        let tool_name = parse::optional_str(&data, "tool_name");
        let hook_event = parse::optional_str(&data, "hook_event_name");

        let tool_class = tool_name
            .map(|n| bash_tool::classify_tool(Agent::Kiro, n))
            .unwrap_or(ToolClass::Skip);
        let is_bash = tool_class == ToolClass::Bash;
        let is_file_edit = tool_class == ToolClass::FileEdit;

        // Kiro doesn't expose the active model in the hook payload.
        // Default to "unknown" rather than reading agent config files.
        let context = PresetContext {
            agent_id: AgentId {
                tool: "kiro".to_string(),
                id: session_id.clone(),
                model: "unknown".to_string(),
            },
            external_session_id: session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::new(),
        };

        let event = match hook_event {
            Some("preToolUse") => {
                if is_bash {
                    ParsedHookEvent::PreBashCall(PreBashCall {
                        context,
                        tool_use_id: "bash".to_string(),
                    })
                } else if is_file_edit {
                    ParsedHookEvent::PreFileEdit(PreFileEdit {
                        context,
                        file_paths: parse::file_paths_from_tool_input(&data, cwd),
                        dirty_files: None,
                        tool_use_id: None,
                    })
                } else {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Kiro preToolUse for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }
            }
            Some("postToolUse") => {
                if is_bash {
                    ParsedHookEvent::PostBashCall(PostBashCall {
                        context,
                        tool_use_id: "bash".to_string(),
                        // Transcript reader for Kiro's SQLite session
                        // storage is not yet implemented; setting None
                        // avoids feeding an undocumented format to the
                        // existing readers.
                        transcript_source: None,
                    })
                } else if is_file_edit {
                    ParsedHookEvent::PostFileEdit(PostFileEdit {
                        context,
                        file_paths: parse::file_paths_from_tool_input(&data, cwd),
                        dirty_files: None,
                        transcript_source: None,
                        tool_use_id: None,
                    })
                } else {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Kiro postToolUse for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }
            }
            _ => {
                return Err(GitAiError::PresetError(format!(
                    "Unsupported Kiro hook_event_name: {}",
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
            "cwd": "/home/user/project",
            "session_id": "f2946a26-3735-4b08-8d05-c928010302d5",
            "tool_name": tool,
            "tool_input": tool_input,
        })
        .to_string()
    }

    #[test]
    fn test_kiro_pre_file_edit_fs_write() {
        let input = make_hook_input(
            "preToolUse",
            "fs_write",
            json!({"path": "src/main.rs", "content": "fn main() {}"}),
        );
        let events = KiroPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "kiro");
                assert_eq!(
                    e.context.agent_id.id,
                    "f2946a26-3735-4b08-8d05-c928010302d5"
                );
                assert_eq!(e.context.agent_id.model, "unknown");
                assert_eq!(
                    e.context.external_session_id,
                    "f2946a26-3735-4b08-8d05-c928010302d5"
                );
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
    fn test_kiro_post_file_edit_fs_write_alias() {
        // Kiro accepts `write` as an alias for `fs_write` per the docs.
        let input = make_hook_input(
            "postToolUse",
            "write",
            json!({"path": "/home/user/project/lib.rs", "content": "..."}),
        );
        let events = KiroPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "kiro");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/lib.rs")]
                );
                assert!(e.transcript_source.is_none());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_kiro_pre_bash_call_execute_bash() {
        let input = make_hook_input("preToolUse", "execute_bash", json!({"command": "ls -la"}));
        let events = KiroPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "kiro");
                assert_eq!(e.tool_use_id, "bash");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_kiro_post_bash_call_shell_alias() {
        // Kiro accepts `shell` as an alias for `execute_bash`.
        let input = make_hook_input("postToolUse", "shell", json!({"command": "ls"}));
        let events = KiroPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "kiro");
                assert!(e.transcript_source.is_none());
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_kiro_unsupported_tool_pre_errors() {
        // fs_read and use_aws are documented but skipped.
        for tool in ["fs_read", "read", "use_aws", "aws", "@git/status"] {
            let input = make_hook_input("preToolUse", tool, json!({}));
            let result = KiroPreset.parse(&input, "t_test");
            assert!(result.is_err(), "expected error for {tool}");
            match result {
                Err(GitAiError::PresetError(msg)) => {
                    assert!(
                        msg.contains("preToolUse for unsupported tool"),
                        "got: {} for {}",
                        msg,
                        tool
                    );
                }
                _ => panic!("Expected PresetError for {tool}"),
            }
        }
    }

    #[test]
    fn test_kiro_lifecycle_event_errors() {
        for event in ["agentSpawn", "userPromptSubmit", "stop"] {
            let input = json!({
                "hook_event_name": event,
                "cwd": "/tmp",
                "session_id": "sess-rej",
            })
            .to_string();
            let result = KiroPreset.parse(&input, "t_test");
            assert!(result.is_err(), "expected error for {event}");
            match result {
                Err(GitAiError::PresetError(msg)) => {
                    assert!(
                        msg.contains("Unsupported Kiro hook_event_name"),
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
    fn test_kiro_camelcase_pre_tool_use_must_match_exactly() {
        // PascalCase Claude-style names must NOT match Kiro's
        // camelCase convention.
        let input = make_hook_input(
            "PreToolUse",
            "fs_write",
            json!({"path": "src/main.rs", "content": "x"}),
        );
        let result = KiroPreset.parse(&input, "t_test");
        assert!(result.is_err(), "PascalCase PreToolUse should not match");
    }

    #[test]
    fn test_kiro_missing_session_id_errors() {
        let input = json!({
            "hook_event_name": "postToolUse",
            "cwd": "/tmp",
            "tool_name": "fs_write",
            "tool_input": {"path": "x"},
        })
        .to_string();
        let result = KiroPreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("session_id"));
    }

    #[test]
    fn test_kiro_missing_cwd_errors() {
        let input = json!({
            "hook_event_name": "postToolUse",
            "session_id": "sess-1",
            "tool_name": "fs_write",
            "tool_input": {"path": "x"},
        })
        .to_string();
        let result = KiroPreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cwd"));
    }

    #[test]
    fn test_kiro_invalid_json_errors() {
        let result = KiroPreset.parse("not json", "t_test");
        assert!(result.is_err());
    }

    #[test]
    fn test_kiro_absolute_path_passes_through() {
        let input = make_hook_input(
            "postToolUse",
            "fs_write",
            json!({"path": "/etc/hosts", "content": ""}),
        );
        let events = KiroPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.file_paths, vec![PathBuf::from("/etc/hosts")]);
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }
}
