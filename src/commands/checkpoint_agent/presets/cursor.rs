use super::parse;
use super::{
    AgentPreset, ParsedHookEvent, PostFileEdit, PreFileEdit, PresetContext, TranscriptFormat,
    TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct CursorPreset;

impl AgentPreset for CursorPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        // conversation_id is required for session_id
        let conversation_id = parse::required_str(&data, "conversation_id")?.to_string();

        // workspace_roots array — first element is default cwd
        let workspace_roots = data
            .get("workspace_roots")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                GitAiError::PresetError("workspace_roots not found in hook_input".to_string())
            })?
            .iter()
            .filter_map(|v| v.as_str().map(normalize_cursor_path))
            .collect::<Vec<String>>();

        let hook_event_name = parse::required_str(&data, "hook_event_name")?;

        // Extract model from hook input (Cursor provides this directly)
        let model = parse::optional_str(&data, "model")
            .unwrap_or("unknown")
            .to_string();

        // Legacy hooks no longer installed; return error so orchestrator skips.
        if hook_event_name == "beforeSubmitPrompt" || hook_event_name == "afterFileEdit" {
            return Err(GitAiError::PresetError(
                "Legacy Cursor hook events (beforeSubmitPrompt/afterFileEdit) are no longer supported."
                    .to_string(),
            ));
        }

        // Validate hook_event_name
        if hook_event_name != "preToolUse" && hook_event_name != "postToolUse" {
            return Err(GitAiError::PresetError(format!(
                "Invalid hook_event_name: {}. Expected 'preToolUse' or 'postToolUse'",
                hook_event_name
            )));
        }

        // Only checkpoint on file-mutating tools (Write, Delete, StrReplace)
        let tool_name = parse::optional_str(&data, "tool_name").unwrap_or("");
        if !matches!(tool_name, "Write" | "Delete" | "StrReplace") {
            return Err(GitAiError::PresetError(format!(
                "Skipping Cursor hook for non-edit tool_name '{}'.",
                tool_name
            )));
        }

        // Extract file_path from tool_input
        let file_path = data
            .get("tool_input")
            .and_then(|ti| ti.get("file_path"))
            .and_then(|v| v.as_str())
            .map(normalize_cursor_path)
            .unwrap_or_default();

        // Resolve cwd: match file_path to workspace root, or fall back to first root
        let cwd = resolve_repo_cwd(&file_path, &workspace_roots).ok_or_else(|| {
            GitAiError::PresetError("No workspace root found in hook_input".to_string())
        })?;

        let file_paths = if !file_path.is_empty() {
            vec![parse::resolve_absolute(&file_path, &cwd)]
        } else {
            vec![]
        };

        let transcript_path = parse::optional_str(&data, "transcript_path").map(|s| s.to_string());

        let mut metadata = HashMap::new();
        if let Some(ref tp) = transcript_path {
            metadata.insert("transcript_path".to_string(), tp.clone());
        }

        let context = PresetContext {
            agent_id: AgentId {
                tool: "cursor".to_string(),
                id: conversation_id.clone(),
                model,
            },
            session_id: conversation_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(&cwd),
            metadata,
        };

        let transcript_source = transcript_path.map(|tp| TranscriptSource::Path {
            path: PathBuf::from(tp),
            format: TranscriptFormat::CursorJsonl,
            session_id: None,
        });

        let event = if hook_event_name == "preToolUse" {
            ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths,
                dirty_files: None,
            })
        } else {
            ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths,
                dirty_files: None,
                transcript_source,
            })
        };

        Ok(vec![event])
    }
}

/// Normalize Windows paths that Cursor sends in Unix-style format.
///
/// On Windows, Cursor sometimes sends paths like `/c:/Users/...` instead of `C:\Users\...`.
/// This function converts those paths to proper Windows format.
#[cfg(windows)]
fn normalize_cursor_path(path: &str) -> String {
    let mut chars = path.chars();
    if chars.next() == Some('/')
        && let (Some(drive), Some(':')) = (chars.next(), chars.next())
        && drive.is_ascii_alphabetic()
    {
        let rest: String = chars.collect();
        let normalized_rest = rest.replace('/', "\\");
        return format!("{}:{}", drive.to_ascii_uppercase(), normalized_rest);
    }
    path.to_string()
}

#[cfg(not(windows))]
fn normalize_cursor_path(path: &str) -> String {
    path.to_string()
}

/// Find the workspace root that matches the given file path.
fn matching_workspace_root(file_path: &str, workspace_roots: &[String]) -> Option<String> {
    workspace_roots
        .iter()
        .find(|root| {
            let root_str = root.as_str();
            file_path.starts_with(root_str)
                && (file_path.len() == root_str.len()
                    || file_path[root_str.len()..].starts_with('/')
                    || file_path[root_str.len()..].starts_with('\\')
                    || root_str.ends_with('/')
                    || root_str.ends_with('\\'))
        })
        .cloned()
}

/// Resolve the cwd for a Cursor hook based on file_path and workspace_roots.
/// Falls back to the first workspace root if no match is found.
fn resolve_repo_cwd(file_path: &str, workspace_roots: &[String]) -> Option<String> {
    if file_path.is_empty() {
        return workspace_roots.first().cloned();
    }
    matching_workspace_root(file_path, workspace_roots).or_else(|| workspace_roots.first().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::checkpoint_agent::presets::*;
    use serde_json::json;

    fn make_cursor_hook_input(event: &str, tool: &str) -> String {
        json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": event,
            "tool_name": tool,
            "model": "claude-3-5-sonnet",
            "transcript_path": "/home/user/.cursor/transcripts/conv-123.jsonl",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string()
    }

    #[test]
    fn test_cursor_pre_file_edit() {
        let input = make_cursor_hook_input("preToolUse", "Write");
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "cursor");
                assert_eq!(e.context.session_id, "conv-123");
                assert_eq!(e.context.trace_id, "t_test123456789a");
                assert_eq!(e.context.agent_id.model, "claude-3-5-sonnet");
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
    fn test_cursor_post_file_edit() {
        let input = make_cursor_hook_input("postToolUse", "Write");
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "cursor");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
                assert!(matches!(
                    e.transcript_source,
                    Some(TranscriptSource::Path {
                        format: TranscriptFormat::CursorJsonl,
                        ..
                    })
                ));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_cursor_skips_non_edit_tools() {
        let input = json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "preToolUse",
            "tool_name": "Read",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string();
        let result = CursorPreset.parse(&input, "t_test123456789a");
        assert!(result.is_err());
    }

    #[test]
    fn test_cursor_skips_legacy_events() {
        let input = json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "beforeSubmitPrompt",
        })
        .to_string();
        let result = CursorPreset.parse(&input, "t_test123456789a");
        assert!(result.is_err());
    }

    #[test]
    fn test_cursor_requires_conversation_id() {
        let input = json!({
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "preToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string();
        let result = CursorPreset.parse(&input, "t_test123456789a");
        assert!(result.is_err());
    }

    #[test]
    fn test_cursor_absolute_file_path() {
        let input = json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "preToolUse",
            "tool_name": "StrReplace",
            "tool_input": {"file_path": "/home/user/project/src/lib.rs"}
        })
        .to_string();
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/lib.rs")]
                );
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_cursor_no_transcript_path() {
        let input = json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "postToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string();
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert!(e.transcript_source.is_none());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_cursor_multiple_workspace_roots() {
        let input = json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project-a", "/home/user/project-b"],
            "hook_event_name": "preToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "/home/user/project-b/src/main.rs"}
        })
        .to_string();
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                // Should pick project-b as cwd since file is there
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project-b"));
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_cursor_delete_tool() {
        let input = make_cursor_hook_input("postToolUse", "Delete");
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ParsedHookEvent::PostFileEdit(_)));
    }

    #[test]
    fn test_matching_workspace_root() {
        let roots = vec![
            "/home/user/project-a".to_string(),
            "/home/user/project-b".to_string(),
        ];
        assert_eq!(
            matching_workspace_root("/home/user/project-b/src/main.rs", &roots),
            Some("/home/user/project-b".to_string())
        );
        assert_eq!(matching_workspace_root("/other/path/file.rs", &roots), None);
    }
}
