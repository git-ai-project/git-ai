//! Augment Code (Auggie CLI) preset.
//!
//! Augment's hook protocol delivers a single-line UTF-8 JSON payload on
//! stdin per the docs at <https://docs.augmentcode.com/cli/hooks>. The
//! top-level fields are:
//!
//!   - `hook_event_name` — `"PreToolUse"`, `"PostToolUse"`, `"SessionStart"`,
//!     `"SessionEnd"`, `"Stop"`
//!   - `conversation_id` — opaque session identifier (NOT `session_id`)
//!   - `workspace_roots` — array of workspace paths (NOT scalar `cwd`)
//!
//! Per-event additions:
//!   - `PreToolUse`: `tool_name`, `tool_input`
//!   - `PostToolUse`: `tool_name`, `tool_input`, `tool_output`,
//!     `tool_error`, `file_changes[]`
//!   - `Stop`: `agent_stop_cause`
//!
//! Tool naming differs from Claude (kebab-case lowercase):
//!   - `save-file` / `str-replace-editor` → file edit (field: `tool_input.path`)
//!   - `remove-files` → file edit (field: `tool_input.file_paths`, plural array)
//!   - `launch-process` → bash (field: `tool_input.command`)
//!
//! Notably **absent** from the payload (verified against the public docs):
//!   - `transcript_path` — Augment does not surface a transcript file
//!   - `session_id` — Augment uses `conversation_id` instead
//!   - `cwd` (scalar) — Augment uses `workspace_roots` array
//!
//! Path resolution: `tool_input.path` is workspace-relative per the docs'
//! jq examples; resolve against `workspace_roots[0]` to get an absolute
//! path. Already-absolute paths are passed through unchanged.
//!
//! **Events not handled:** `SessionStart`, `SessionEnd`, `Stop`. The
//! installer subscribes only to `PreToolUse` and `PostToolUse` (see
//! `mdm/agents/augment.rs::AUGMENT_HOOK_EVENTS`), so these other events
//! should not normally reach this preset. We defensively reject any
//! event other than `PreToolUse`/`PostToolUse` with an explicit
//! `PresetError`.
//!
//! Transcript reading: not implemented. Augment exposes
//! `conversation.agentCodeResponse[]` only on `Stop` when
//! `metadata.includeConversationData` is enabled, but no on-disk
//! transcript file is documented. `transcript_source` is therefore set
//! to `None`. Hook-based attribution still works without it; a
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

pub struct AugmentPreset;

fn extract_augment_file_paths(data: &serde_json::Value, workspace_root: &str) -> Vec<PathBuf> {
    let tool_input = match data.get("tool_input") {
        Some(ti) => ti,
        None => return vec![],
    };

    // `remove-files` sends `file_paths` as an array.
    if let Some(arr) = tool_input.get("file_paths").and_then(|v| v.as_array()) {
        let paths: Vec<PathBuf> = arr
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|p| parse::resolve_absolute(p, workspace_root))
            .collect();
        if !paths.is_empty() {
            return paths;
        }
    }

    // `save-file` and `str-replace-editor` send `path`.
    if let Some(path) = tool_input.get("path").and_then(|v| v.as_str())
        && !path.is_empty()
    {
        return vec![parse::resolve_absolute(path, workspace_root)];
    }

    vec![]
}

impl AgentPreset for AugmentPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let conversation_id = parse::required_str(&data, "conversation_id")?.to_string();

        // workspace_roots is required; use the first entry as the canonical cwd.
        let workspace_root = data
            .get("workspace_roots")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "workspace_roots[0] not found in Augment hook_input".to_string(),
                )
            })?
            .to_string();

        let tool_name = parse::optional_str(&data, "tool_name");
        let hook_event = parse::optional_str(&data, "hook_event_name");
        // Augment does not document a per-tool-call id field, so default
        // to "bash" for bash tools (matching the Claude/Codex pattern)
        // and "unknown" for file-edit events.
        let tool_class = tool_name
            .map(|n| bash_tool::classify_tool(Agent::Augment, n))
            .unwrap_or(ToolClass::Skip);
        let is_bash = tool_class == ToolClass::Bash;
        let is_file_edit = tool_class == ToolClass::FileEdit;

        // Augment exposes `context.modelName` only when
        // `metadata.includeUserContext: true` is set in the hook config.
        // Default to "unknown" when absent rather than reading agent
        // config files.
        let model = data
            .get("context")
            .and_then(|c| c.get("modelName"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown")
            .to_string();

        let context = PresetContext {
            agent_id: AgentId {
                tool: "augment".to_string(),
                id: conversation_id.clone(),
                model,
            },
            external_session_id: conversation_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(&workspace_root),
            metadata: HashMap::new(),
        };

        // Mirror the codex/kimi-code style: explicit error on unknown events
        // so we never fabricate spurious file-edit checkpoints when Augment
        // fires SessionStart/SessionEnd/Stop.
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
                        file_paths: extract_augment_file_paths(&data, &workspace_root),
                        dirty_files: None,
                        tool_use_id: None,
                    })
                } else {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Augment PreToolUse for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }
            }
            Some("PostToolUse") => {
                if is_bash {
                    ParsedHookEvent::PostBashCall(PostBashCall {
                        context,
                        tool_use_id: "bash".to_string(),
                        // Transcript reader for Augment's conversation
                        // history is not yet implemented; setting None
                        // avoids feeding an unsupported format to the
                        // existing readers.
                        transcript_source: None,
                    })
                } else if is_file_edit {
                    // Prefer file_changes[].path on PostToolUse (present
                    // when Augment captured the actual mutation) over
                    // tool_input.path. Falling back to tool_input keeps
                    // us correct when file_changes is absent.
                    let post_paths = extract_post_file_paths(&data, &workspace_root);
                    let file_paths = if post_paths.is_empty() {
                        extract_augment_file_paths(&data, &workspace_root)
                    } else {
                        post_paths
                    };
                    ParsedHookEvent::PostFileEdit(PostFileEdit {
                        context,
                        file_paths,
                        dirty_files: None,
                        transcript_source: None,
                        tool_use_id: None,
                    })
                } else {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Augment PostToolUse for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }
            }
            _ => {
                return Err(GitAiError::PresetError(format!(
                    "Unsupported Augment hook_event_name: {}",
                    hook_event.unwrap_or("<missing>")
                )));
            }
        };

        Ok(vec![event])
    }
}

fn extract_post_file_paths(data: &serde_json::Value, workspace_root: &str) -> Vec<PathBuf> {
    data.get("file_changes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|change| change.get("path").and_then(|p| p.as_str()))
                .filter(|s| !s.is_empty())
                .map(|p| parse::resolve_absolute(p, workspace_root))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::checkpoint_agent::presets::*;
    use serde_json::json;

    fn make_hook_input(event: &str, tool: &str, tool_input: serde_json::Value) -> String {
        json!({
            "hook_event_name": event,
            "conversation_id": "conv-xyz789",
            "workspace_roots": ["/Users/me/project"],
            "tool_name": tool,
            "tool_input": tool_input,
        })
        .to_string()
    }

    #[test]
    fn test_augment_pre_file_edit_save_file() {
        let input = make_hook_input(
            "PreToolUse",
            "save-file",
            json!({"path": "src/main.rs", "content": "fn main() {}"}),
        );
        let events = AugmentPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "augment");
                assert_eq!(e.context.agent_id.id, "conv-xyz789");
                assert_eq!(e.context.agent_id.model, "unknown");
                assert_eq!(e.context.external_session_id, "conv-xyz789");
                assert_eq!(e.context.trace_id, "t_test123456789a");
                assert_eq!(e.context.cwd, PathBuf::from("/Users/me/project"));
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/Users/me/project/src/main.rs")]
                );
                assert!(e.dirty_files.is_none());
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_augment_post_file_edit_str_replace_editor() {
        let input = make_hook_input(
            "PostToolUse",
            "str-replace-editor",
            json!({
                "path": "src/lib.rs",
                "command": "str_replace",
                "old_str_1": "a",
                "new_str_1": "b",
            }),
        );
        let events = AugmentPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "augment");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/Users/me/project/src/lib.rs")]
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
    fn test_augment_post_file_edit_uses_file_changes_when_present() {
        // Augment's PostToolUse can include file_changes[] which is the
        // authoritative list of mutated files. Prefer it over tool_input.
        let input = json!({
            "hook_event_name": "PostToolUse",
            "conversation_id": "conv-1",
            "workspace_roots": ["/Users/me/project"],
            "tool_name": "save-file",
            "tool_input": {"path": "src/old.rs"},
            "file_changes": [
                {"path": "src/new.rs", "changeType": "create"},
                {"path": "src/also.rs", "changeType": "modify"},
            ],
        })
        .to_string();
        let events = AugmentPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(
                    e.file_paths,
                    vec![
                        PathBuf::from("/Users/me/project/src/new.rs"),
                        PathBuf::from("/Users/me/project/src/also.rs"),
                    ]
                );
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_augment_remove_files_uses_file_paths_array() {
        // remove-files uses `file_paths` (plural array), not `path`.
        let input = make_hook_input(
            "PostToolUse",
            "remove-files",
            json!({"file_paths": ["src/dead.rs", "src/old.rs"]}),
        );
        let events = AugmentPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(
                    e.file_paths,
                    vec![
                        PathBuf::from("/Users/me/project/src/dead.rs"),
                        PathBuf::from("/Users/me/project/src/old.rs"),
                    ]
                );
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_augment_pre_bash_call() {
        let input = make_hook_input(
            "PreToolUse",
            "launch-process",
            json!({"command": "git status"}),
        );
        let events = AugmentPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "augment");
                assert_eq!(e.tool_use_id, "bash");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_augment_post_bash_call() {
        let input = make_hook_input("PostToolUse", "launch-process", json!({"command": "ls"}));
        let events = AugmentPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "augment");
                assert!(e.transcript_source.is_none());
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_augment_absolute_path_passes_through() {
        let input = make_hook_input(
            "PostToolUse",
            "save-file",
            json!({"path": "/etc/hosts", "content": ""}),
        );
        let events = AugmentPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                // Already absolute — must not be re-rooted under workspace.
                assert_eq!(e.file_paths, vec![PathBuf::from("/etc/hosts")]);
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_augment_unsupported_tool_pretooluse_errors() {
        // Tools like view, grep-search, codebase-retrieval, web-fetch are
        // documented but we don't checkpoint for them — verify they error
        // rather than silently fabricating a checkpoint.
        let input = make_hook_input("PreToolUse", "view", json!({"path": "src/foo.rs"}));
        let result = AugmentPreset.parse(&input, "t_test");
        assert!(result.is_err());
        match result {
            Err(GitAiError::PresetError(msg)) => {
                assert!(
                    msg.contains("PreToolUse for unsupported tool"),
                    "expected unsupported-tool message, got: {}",
                    msg
                );
            }
            _ => panic!("Expected PresetError"),
        }
    }

    #[test]
    fn test_augment_unsupported_tool_posttooluse_errors() {
        let input = make_hook_input("PostToolUse", "web-fetch", json!({"url": "https://x"}));
        let result = AugmentPreset.parse(&input, "t_test");
        assert!(result.is_err());
    }

    #[test]
    fn test_augment_lifecycle_event_errors() {
        // SessionStart / SessionEnd / Stop must not silently fall through
        // to PostFileEdit.
        for event in ["SessionStart", "SessionEnd", "Stop"] {
            let input = json!({
                "hook_event_name": event,
                "conversation_id": "conv-1",
                "workspace_roots": ["/Users/me/project"],
            })
            .to_string();
            let result = AugmentPreset.parse(&input, "t_test");
            assert!(result.is_err(), "expected error for {event}");
            match result {
                Err(GitAiError::PresetError(msg)) => {
                    assert!(
                        msg.contains("Unsupported Augment hook_event_name"),
                        "expected unknown-event message for {event}, got: {}",
                        msg
                    );
                }
                _ => panic!("Expected PresetError for {event}"),
            }
        }
    }

    #[test]
    fn test_augment_missing_event_name_errors() {
        let input = json!({
            "conversation_id": "conv-1",
            "workspace_roots": ["/Users/me/project"],
        })
        .to_string();
        let result = AugmentPreset.parse(&input, "t_test");
        assert!(result.is_err());
    }

    #[test]
    fn test_augment_invalid_json_errors() {
        let result = AugmentPreset.parse("not valid json", "t_test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid JSON in hook_input")
        );
    }

    #[test]
    fn test_augment_missing_conversation_id_errors() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "workspace_roots": ["/Users/me/project"],
            "tool_name": "save-file",
            "tool_input": {"path": "x"},
        })
        .to_string();
        let result = AugmentPreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("conversation_id"));
    }

    #[test]
    fn test_augment_missing_workspace_roots_errors() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "conversation_id": "conv-1",
            "tool_name": "save-file",
            "tool_input": {"path": "x"},
        })
        .to_string();
        let result = AugmentPreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("workspace_roots"));
    }

    #[test]
    fn test_augment_extracts_model_when_context_present() {
        // Augment exposes context.modelName only with metadata.includeUserContext.
        let input = json!({
            "hook_event_name": "PostToolUse",
            "conversation_id": "conv-1",
            "workspace_roots": ["/Users/me/project"],
            "tool_name": "save-file",
            "tool_input": {"path": "src/main.rs"},
            "context": {"modelName": "claude-sonnet-4-5"},
        })
        .to_string();
        let events = AugmentPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.model, "claude-sonnet-4-5");
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }
}
