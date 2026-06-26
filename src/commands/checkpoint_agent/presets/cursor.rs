use super::parse;
use super::{
    AgentPreset, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    PresetContext, StreamFormat, StreamSource,
};
use crate::authorship::authorship_log_serialization::generate_session_id;
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

        // Extract the edited path from Cursor file-edit tool input.
        let file_path = cursor_file_path_from_tool_input(data.get("tool_input"));

        // For ApplyPatch, extract file paths from patch text if no direct file_path.
        let patch_paths = if file_path.is_empty() && tool_name == "ApplyPatch" {
            extract_paths_from_patch(data.get("tool_input"))
        } else {
            vec![]
        };

        // Resolve cwd: match file_path to workspace root, or fall back to first root.
        // For Shell tools `file_path` is empty, so this returns workspace_roots[0].
        let first_path = if !file_path.is_empty() {
            &file_path
        } else {
            patch_paths.first().map(|s| s.as_str()).unwrap_or("")
        };
        let cwd = resolve_repo_cwd(first_path, &workspace_roots).ok_or_else(|| {
            GitAiError::PresetError("No workspace root found in hook_input".to_string())
        })?;

        let file_paths = if !file_path.is_empty() {
            vec![parse::resolve_absolute(&file_path, &cwd)]
        } else if !patch_paths.is_empty() {
            patch_paths
                .iter()
                .map(|p| parse::resolve_absolute(p, &cwd))
                .collect()
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
            external_session_id: conversation_id.clone(),
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(&cwd),
            metadata,
        };

        let stream_source = transcript_path.map(|tp| StreamSource {
            path: PathBuf::from(tp),
            format: StreamFormat::CursorJsonl,
            session_id: generate_session_id(&conversation_id, "cursor"),
            external_session_id: conversation_id.clone(),
            external_parent_session_id: None,
        });

        let is_pre = hook_event_name == "preToolUse";
        let tool_use_id = parse::optional_str(&data, "tool_use_id")
            .unwrap_or("bash")
            .to_string();

        let bash_command = parse::bash_command_from_hook_input(&data);
        let event = match (tool_class, is_pre) {
            (ToolClass::Bash, true) => ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id,
                command: bash_command.clone(),
            }),
            (ToolClass::Bash, false) => ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id,
                command: bash_command,
                stream_source,
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
                stream_source,
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

/// Extract file paths from an ApplyPatch tool_input's patch text.
/// Delegates to the shared `parse::collect_apply_patch_paths_from_text` helper,
/// then applies `normalize_cursor_path` so patch-extracted paths get the same
/// Windows `/c:/...` -> `C:\...` normalization as JSON-field paths.
fn extract_paths_from_patch(tool_input: Option<&serde_json::Value>) -> Vec<String> {
    let mut paths = Vec::new();
    let patch_text = tool_input.and_then(|ti| {
        ti.as_str()
            .or_else(|| ti.get("patch").and_then(|v| v.as_str()))
    });
    if let Some(text) = patch_text {
        parse::collect_apply_patch_paths_from_text(text, &mut paths);
    }
    paths
        .into_iter()
        .map(|p| normalize_cursor_path(&p))
        .collect()
}

fn cursor_file_path_from_tool_input(tool_input: Option<&serde_json::Value>) -> String {
    tool_input
        .and_then(|ti| {
            ["file_path", "path", "filePath"]
                .iter()
                .find_map(|key| ti.get(key).and_then(|v| v.as_str()))
        })
        .map(normalize_cursor_path)
        .unwrap_or_default()
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
                assert_eq!(e.context.external_session_id, "conv-123");
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
                assert!(e.stream_source.is_some());
                if let Some(ts) = &e.stream_source {
                    assert_eq!(ts.format, StreamFormat::CursorJsonl);
                    assert_eq!(ts.session_id, generate_session_id("conv-123", "cursor"));
                    assert_eq!(ts.external_session_id, "conv-123");
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
    fn test_cursor_file_edit_accepts_path_field() {
        let input = json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "preToolUse",
            "tool_name": "StrReplace",
            "tool_input": {"path": "/home/user/project/src/lib.rs"}
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
    fn test_cursor_file_edit_prefers_file_path_over_path() {
        let input = json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "preToolUse",
            "tool_name": "StrReplace",
            "tool_input": {
                "file_path": "src/from_file_path.rs",
                "path": "src/from_path.rs"
            }
        })
        .to_string();
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/from_file_path.rs")]
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
                assert!(e.stream_source.is_none());
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
                assert_eq!(e.context.external_session_id, "conv-shell");
                assert_eq!(e.context.agent_id.model, "composer-2");
                assert_eq!(
                    e.context.cwd,
                    PathBuf::from("/Users/aidan/Desktop/test-repo")
                );
                assert_eq!(e.tool_use_id, "tu-shell-1");
                assert_eq!(e.command.as_deref(), Some("date > current_time.txt"));
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
                assert_eq!(e.command.as_deref(), Some("date > current_time.txt"));
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
    fn test_cursor_apply_patch_pre_file_edit() {
        let input = json!({
            "conversation_id": "conv-patch",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "preToolUse",
            "tool_name": "ApplyPatch",
            "model": "claude-3-5-sonnet",
            "tool_input": {
                "patch": "*** Begin Patch\n*** Update File: src/example.rs\n@@\n-old\n+new\n*** End Patch\n"
            }
        })
        .to_string();
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "cursor");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/example.rs")]
                );
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_cursor_apply_patch_post_file_edit() {
        let input = json!({
            "conversation_id": "conv-patch",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "postToolUse",
            "tool_name": "ApplyPatch",
            "model": "claude-3-5-sonnet",
            "transcript_path": "/home/user/.cursor/transcripts/conv-patch.jsonl",
            "tool_input": {
                "patch": "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old line\n+new line\n*** Add File: src/new.rs\n@@\n+content\n*** End Patch\n"
            }
        })
        .to_string();
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "cursor");
                assert_eq!(e.file_paths.len(), 2);
                assert_eq!(
                    e.file_paths[0],
                    PathBuf::from("/home/user/project/src/main.rs")
                );
                assert_eq!(
                    e.file_paths[1],
                    PathBuf::from("/home/user/project/src/new.rs")
                );
                assert!(e.stream_source.is_some());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_cursor_apply_patch_with_absolute_path_in_patch() {
        let input = json!({
            "conversation_id": "conv-patch",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "preToolUse",
            "tool_name": "ApplyPatch",
            "tool_input": {
                "patch": "*** Update File: /home/user/project/src/lib.rs\nsome diff"
            }
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

    #[cfg(windows)]
    #[test]
    fn test_cursor_apply_patch_normalizes_windows_path() {
        // Cursor can embed Unix-style `/c:/...` paths in patch text on Windows;
        // patch-extracted paths must be normalized just like JSON-field paths.
        let input = json!({
            "conversation_id": "conv-patch",
            "workspace_roots": ["C:\\Users\\project"],
            "hook_event_name": "preToolUse",
            "tool_name": "ApplyPatch",
            "tool_input": {
                "patch": "*** Update File: /c:/Users/project/src/main.rs\nsome diff"
            }
        })
        .to_string();
        let events = CursorPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("C:\\Users\\project\\src\\main.rs")]
                );
            }
            _ => panic!("Expected PreFileEdit"),
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
