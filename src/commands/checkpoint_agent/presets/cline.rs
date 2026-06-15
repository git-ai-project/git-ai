//! Cline (cline.bot VS Code extension) preset.
//!
//! Cline's hook protocol delivers a single-line UTF-8 JSON payload on
//! stdin per the docs at <https://docs.cline.bot/customization/hooks>.
//! The wire schema (verified against the public docs):
//!
//! ```json
//! {
//!   "taskId":         "string",
//!   "hookName":       "PreToolUse",
//!   "clineVersion":   "3.17.0",
//!   "timestamp":      "1730000000000",
//!   "workspaceRoots": ["/path/to/workspace"],
//!   "userId":         "string",
//!   "model":          {"provider": "anthropic", "slug": "claude-sonnet-4-5"},
//!   "toolName":       "write_to_file",
//!   "parameters":     {"path": "src/main.rs", "content": "..."}
//! }
//! ```
//!
//! Notable differences from Claude:
//!   - Top-level fields use camelCase (`taskId`, `hookName`,
//!     `workspaceRoots`, `toolName`, `clineVersion`).
//!   - `taskId` (NOT `session_id`).
//!   - `workspaceRoots` array (NOT scalar `cwd`).
//!   - `toolName` is snake_case (`write_to_file`, `execute_command`,
//!     etc.) — Claude uses PascalCase.
//!   - `parameters` (NOT `tool_input`).
//!   - `model` is an object with `provider` and `slug` (Claude uses a
//!     scalar string).
//!
//! Tool naming (snake_case lowercase):
//!   - `write_to_file` / `replace_in_file` → file edit (field:
//!     `parameters.path`)
//!   - `execute_command`                   → bash (field:
//!     `parameters.command`)
//!   - `read_file` / `search_files` /
//!     `list_files` / `list_code_definition_names` /
//!     `browser_action` / `use_mcp_tool` /
//!     `access_mcp_resource` / `ask_followup_question` /
//!     `attempt_completion` / `new_task`  → read-only / non-mutating skip
//!
//! Path resolution: `parameters.path` is workspace-relative per the
//! Cline tool reference; resolve against `workspaceRoots[0]` to get
//! an absolute path. Already-absolute paths pass through unchanged.
//!
//! **Events not handled:** `TaskStart`, `TaskResume`, `TaskCancel`,
//! `TaskComplete`, `UserPromptSubmit`, `PreCompact`. The installer
//! drops only `PreToolUse` and `PostToolUse` hook scripts (see
//! `mdm/agents/cline.rs::CLINE_HOOK_EVENTS`), so these other events
//! should not normally reach this preset. We defensively reject any
//! event other than these two with an explicit `PresetError`.
//!
//! Transcript reading: not implemented. Cline's hook payload doesn't
//! include a transcript path, and the conversation history is held in
//! the VS Code extension's webview state — not on disk in a documented
//! format. `transcript_source` is therefore set to `None`. Hook-based
//! attribution still works without it; a dedicated reader can land in
//! a follow-up PR.

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

pub struct ClinePreset;

fn extract_cline_file_paths(data: &serde_json::Value, workspace_root: &str) -> Vec<PathBuf> {
    let parameters = match data.get("parameters") {
        Some(p) => p,
        None => return vec![],
    };

    if let Some(path) = parameters.get("path").and_then(|v| v.as_str())
        && !path.is_empty()
    {
        return vec![parse::resolve_absolute(path, workspace_root)];
    }

    vec![]
}

fn extract_cline_model(data: &serde_json::Value) -> String {
    let Some(model) = data.get("model") else {
        return "unknown".to_string();
    };

    // model is documented as an object with `provider` and `slug` —
    // prefer slug. Tolerate older or alternate shapes (raw string)
    // gracefully.
    if let Some(slug) = model.get("slug").and_then(|v| v.as_str())
        && !slug.is_empty()
    {
        return slug.to_string();
    }
    if let Some(s) = model.as_str()
        && !s.is_empty()
    {
        return s.to_string();
    }
    "unknown".to_string()
}

impl AgentPreset for ClinePreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let task_id = parse::required_str(&data, "taskId")?.to_string();

        let workspace_root = data
            .get("workspaceRoots")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "workspaceRoots[0] not found in Cline hook_input".to_string(),
                )
            })?
            .to_string();

        let tool_name = parse::optional_str(&data, "toolName");
        let hook_event = parse::optional_str(&data, "hookName");

        let tool_class = tool_name
            .map(|n| bash_tool::classify_tool(Agent::Cline, n))
            .unwrap_or(ToolClass::Skip);
        let is_bash = tool_class == ToolClass::Bash;
        let is_file_edit = tool_class == ToolClass::FileEdit;

        let model = extract_cline_model(&data);

        let context = PresetContext {
            agent_id: AgentId {
                tool: "cline".to_string(),
                id: task_id.clone(),
                model,
            },
            external_session_id: task_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(&workspace_root),
            metadata: HashMap::new(),
        };

        let event = match hook_event {
            Some("PreToolUse") => {
                if is_bash {
                    ParsedHookEvent::PreBashCall(PreBashCall {
                        context,
                        tool_use_id: "bash".to_string(),
                    })
                } else if is_file_edit {
                    ParsedHookEvent::PreFileEdit(PreFileEdit {
                        context,
                        file_paths: extract_cline_file_paths(&data, &workspace_root),
                        dirty_files: None,
                        tool_use_id: None,
                    })
                } else {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Cline PreToolUse for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }
            }
            Some("PostToolUse") => {
                if is_bash {
                    ParsedHookEvent::PostBashCall(PostBashCall {
                        context,
                        tool_use_id: "bash".to_string(),
                        // Transcript reader not yet implemented — Cline
                        // holds conversation state in the VS Code
                        // webview, not on disk in a documented format.
                        transcript_source: None,
                    })
                } else if is_file_edit {
                    ParsedHookEvent::PostFileEdit(PostFileEdit {
                        context,
                        file_paths: extract_cline_file_paths(&data, &workspace_root),
                        dirty_files: None,
                        transcript_source: None,
                        tool_use_id: None,
                    })
                } else {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Cline PostToolUse for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }
            }
            _ => {
                return Err(GitAiError::PresetError(format!(
                    "Unsupported Cline hookName: {}",
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

    fn make_hook_input(hook: &str, tool: &str, parameters: serde_json::Value) -> String {
        json!({
            "taskId": "task-abc",
            "hookName": hook,
            "clineVersion": "3.17.0",
            "timestamp": "1730000000000",
            "workspaceRoots": ["/Users/me/project"],
            "userId": "u-1",
            "model": {"provider": "anthropic", "slug": "claude-sonnet-4-5"},
            "toolName": tool,
            "parameters": parameters,
        })
        .to_string()
    }

    #[test]
    fn test_cline_pre_file_edit_write_to_file() {
        let input = make_hook_input(
            "PreToolUse",
            "write_to_file",
            json!({"path": "src/main.rs", "content": "fn main() {}"}),
        );
        let events = ClinePreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "cline");
                assert_eq!(e.context.agent_id.id, "task-abc");
                assert_eq!(e.context.agent_id.model, "claude-sonnet-4-5");
                assert_eq!(e.context.external_session_id, "task-abc");
                assert_eq!(e.context.trace_id, "t_test123456789a");
                assert_eq!(e.context.cwd, PathBuf::from("/Users/me/project"));
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/Users/me/project/src/main.rs")]
                );
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_cline_post_file_edit_replace_in_file() {
        let input = make_hook_input(
            "PostToolUse",
            "replace_in_file",
            json!({"path": "/Users/me/project/lib.rs", "diff": "..."}),
        );
        let events = ClinePreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "cline");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/Users/me/project/lib.rs")]
                );
                assert!(e.transcript_source.is_none());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_cline_routes_execute_command_to_bash() {
        let pre = make_hook_input(
            "PreToolUse",
            "execute_command",
            json!({"command": "ls", "requires_approval": false}),
        );
        let events = ClinePreset.parse(&pre, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "cline");
                assert_eq!(e.tool_use_id, "bash");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_cline_post_bash_call() {
        let post = make_hook_input("PostToolUse", "execute_command", json!({"command": "ls"}));
        let events = ClinePreset.parse(&post, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "cline");
                assert!(e.transcript_source.is_none());
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_cline_absolute_path_passes_through() {
        let input = make_hook_input(
            "PostToolUse",
            "write_to_file",
            json!({"path": "/etc/hosts", "content": ""}),
        );
        let events = ClinePreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.file_paths, vec![PathBuf::from("/etc/hosts")]);
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_cline_unsupported_tool_pre_errors() {
        for tool in [
            "read_file",
            "search_files",
            "list_files",
            "list_code_definition_names",
            "browser_action",
            "use_mcp_tool",
            "access_mcp_resource",
            "ask_followup_question",
            "attempt_completion",
            "new_task",
        ] {
            let input = make_hook_input("PreToolUse", tool, json!({}));
            let result = ClinePreset.parse(&input, "t_test");
            assert!(result.is_err(), "expected error for tool {tool}");
            match result {
                Err(GitAiError::PresetError(msg)) => {
                    assert!(
                        msg.contains("PreToolUse for unsupported tool"),
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
    fn test_cline_lifecycle_event_errors() {
        for hook in [
            "TaskStart",
            "TaskResume",
            "TaskCancel",
            "TaskComplete",
            "UserPromptSubmit",
            "PreCompact",
        ] {
            let input = json!({
                "taskId": "task-rej",
                "hookName": hook,
                "workspaceRoots": ["/Users/me/project"],
            })
            .to_string();
            let result = ClinePreset.parse(&input, "t_test");
            assert!(result.is_err(), "expected error for {hook}");
            match result {
                Err(GitAiError::PresetError(msg)) => {
                    assert!(
                        msg.contains("Unsupported Cline hookName"),
                        "got: {} for {}",
                        msg,
                        hook
                    );
                }
                _ => panic!("Expected PresetError for {hook}"),
            }
        }
    }

    #[test]
    fn test_cline_missing_task_id_errors() {
        let input = json!({
            "hookName": "PostToolUse",
            "workspaceRoots": ["/Users/me/project"],
            "toolName": "write_to_file",
            "parameters": {"path": "x"},
        })
        .to_string();
        let result = ClinePreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("taskId"));
    }

    #[test]
    fn test_cline_missing_workspace_roots_errors() {
        let input = json!({
            "taskId": "t-1",
            "hookName": "PostToolUse",
            "toolName": "write_to_file",
            "parameters": {"path": "x"},
        })
        .to_string();
        let result = ClinePreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("workspaceRoots"));
    }

    #[test]
    fn test_cline_invalid_json_errors() {
        let result = ClinePreset.parse("not json", "t_test");
        assert!(result.is_err());
    }

    #[test]
    fn test_cline_model_defaults_to_unknown_when_missing() {
        let input = json!({
            "taskId": "t-1",
            "hookName": "PostToolUse",
            "workspaceRoots": ["/Users/me/project"],
            "toolName": "write_to_file",
            "parameters": {"path": "x"},
        })
        .to_string();
        let events = ClinePreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.model, "unknown");
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_cline_model_extracted_from_object_slug() {
        let input = make_hook_input(
            "PostToolUse",
            "write_to_file",
            json!({"path": "src/main.rs"}),
        );
        let events = ClinePreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.model, "claude-sonnet-4-5");
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_cline_model_tolerates_string_shape() {
        // Older Cline payloads or alternate clients may send model as
        // a bare string rather than an object. Accept both.
        let input = json!({
            "taskId": "t-1",
            "hookName": "PostToolUse",
            "workspaceRoots": ["/Users/me/project"],
            "toolName": "write_to_file",
            "parameters": {"path": "x"},
            "model": "claude-opus-4-6",
        })
        .to_string();
        let events = ClinePreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.model, "claude-opus-4-6");
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }
}
