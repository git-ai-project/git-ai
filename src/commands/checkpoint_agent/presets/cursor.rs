use super::parse;
use super::{
    AgentPreset, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    PresetContext, TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct CursorPreset;

pub struct CursorBackgroundPreset;

impl AgentPreset for CursorBackgroundPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        if std::env::var("HOSTNAME").ok().as_deref() != Some("cursor") {
            return Err(GitAiError::PresetError(
                "Skipping cursor-background hook outside cursor agent environment.".to_string(),
            ));
        }
        CursorPreset.parse(hook_input, trace_id)
    }
}

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

        // Classify the tool: file-edit (Write/Delete/StrReplace), bash (Shell), or skip.
        let tool_name = parse::optional_str(&data, "tool_name").unwrap_or("");
        let tool_class = bash_tool::classify_tool(Agent::Cursor, tool_name);
        if tool_class == ToolClass::Skip {
            return Err(GitAiError::PresetError(format!(
                "Skipping Cursor hook for unsupported tool_name '{}'.",
                tool_name
            )));
        }

        // Extract file_path from tool_input (file-edit tools only).
        let file_path = data
            .get("tool_input")
            .and_then(|ti| ti.get("file_path"))
            .and_then(|v| v.as_str())
            .map(normalize_cursor_path)
            .unwrap_or_default();

        // Resolve cwd: match file_path to workspace root, or fall back to first root.
        // For Shell tools `file_path` is empty, so this returns workspace_roots[0].
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
                model: model.clone(),
            },
            session_id: conversation_id.clone(),
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(&cwd),
            metadata,
        };

        let transcript_source = transcript_path.map(|tp| TranscriptSource {
            path: PathBuf::from(tp),
            format: TranscriptFormat::CursorJsonl,
            session_id: conversation_id.clone(),
            external_thread_id: Some(conversation_id.clone()),
        });

        let is_pre = hook_event_name == "preToolUse";
        let tool_use_id = parse::optional_str(&data, "tool_use_id")
            .unwrap_or("bash")
            .to_string();

        let event = match (tool_class, is_pre) {
            (ToolClass::Bash, true) => ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id,
            }),
            (ToolClass::Bash, false) => ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id,
                transcript_source,
            }),
            (ToolClass::FileEdit, true) => ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths,
                dirty_files: None,
                tool_use_id: Some(tool_use_id),
            }),
            (ToolClass::FileEdit, false) => ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths,
                dirty_files: None,
                transcript_source,
                tool_use_id: Some(tool_use_id),
            }),
            (ToolClass::Skip, _) => unreachable!("Skip handled above"),
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
                assert!(e.transcript_source.is_some());
                if let Some(ts) = &e.transcript_source {
                    assert_eq!(ts.format, TranscriptFormat::CursorJsonl);
                    assert_eq!(ts.session_id, "conv-123");
                }
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
    fn test_cursor_pre_shell_tool() {
        let input = json!({
            "conversation_id": "conv-shell",
            "session_id": "conv-shell",
            "workspace_roots": ["/Users/aidan/Desktop/test-repo"],
            "hook_event_name": "preToolUse",
            "tool_name": "Shell",
            "tool_use_id": "tu-shell-1",
            "model": "composer-2",
            "cursor_version": "3.1.17",
            "tool_input": {
                "command": "date > current_time.txt",
                "cwd": "",
                "timeout": 30000
            }
        })
        .to_string();
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "cursor");
                assert_eq!(e.context.session_id, "conv-shell");
                assert_eq!(e.context.agent_id.model, "composer-2");
                assert_eq!(
                    e.context.cwd,
                    PathBuf::from("/Users/aidan/Desktop/test-repo")
                );
                assert_eq!(e.tool_use_id, "tu-shell-1");
            }
            _ => panic!("Expected PreBashCall, got {:?}", events[0]),
        }
    }

    #[test]
    fn test_cursor_post_shell_tool() {
        let input = json!({
            "conversation_id": "conv-shell",
            "session_id": "conv-shell",
            "workspace_roots": ["/Users/aidan/Desktop/test-repo"],
            "hook_event_name": "postToolUse",
            "tool_name": "Shell",
            "tool_use_id": "tu-shell-2",
            "model": "composer-2",
            "tool_input": {
                "command": "date > current_time.txt",
                "cwd": ""
            }
        })
        .to_string();
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "cursor");
                assert_eq!(e.tool_use_id, "tu-shell-2");
            }
            _ => panic!("Expected PostBashCall, got {:?}", events[0]),
        }
    }

    #[test]
    fn test_cursor_shell_falls_back_to_default_tool_use_id() {
        let input = json!({
            "conversation_id": "conv-shell",
            "workspace_roots": ["/Users/aidan/Desktop/test-repo"],
            "hook_event_name": "preToolUse",
            "tool_name": "Shell",
            "tool_input": {"command": "ls"}
        })
        .to_string();
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.tool_use_id, "bash");
            }
            _ => panic!("Expected PreBashCall"),
        }
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
