use super::parse;
use super::{
    AgentPreset, BashPreHookStrategy, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall,
    PreFileEdit, PresetContext,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use crate::utils::normalize_to_posix;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct FirebenderPreset;

#[derive(Debug, Deserialize)]
struct FirebenderHookInput {
    hook_event_name: String,
    model: String,
    repo_working_dir: Option<String>,
    workspace_roots: Option<Vec<String>>,
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
    completion_id: Option<String>,
    dirty_files: Option<HashMap<String, String>>,
}

impl FirebenderPreset {
    fn push_unique_path(paths: &mut Vec<String>, candidate: &str) {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() && !paths.iter().any(|path| path == trimmed) {
            paths.push(trimmed.to_string());
        }
    }

    fn normalize_hook_path(raw_path: &str, cwd: &str) -> Option<String> {
        let trimmed = raw_path.trim();
        if trimmed.is_empty() {
            return None;
        }

        let normalized_path = normalize_to_posix(trimmed);
        let normalized_cwd = normalize_to_posix(cwd.trim())
            .trim_end_matches('/')
            .to_string();

        if normalized_cwd.is_empty() {
            return Some(normalized_path);
        }

        let relative = if normalized_path == normalized_cwd {
            String::new()
        } else if let Some(stripped) = normalized_path.strip_prefix(&(normalized_cwd.clone() + "/"))
        {
            stripped.to_string()
        } else {
            normalized_path
        };

        Some(relative)
    }

    fn extract_patch_paths(patch: &str) -> Vec<String> {
        let mut paths = Vec::new();

        for line in patch.lines() {
            for prefix in [
                "*** Add File: ",
                "*** Update File: ",
                "*** Delete File: ",
                "*** Move to: ",
            ] {
                if let Some(path) = line.strip_prefix(prefix) {
                    Self::push_unique_path(&mut paths, path);
                }
            }
        }

        paths
    }

    fn extract_file_paths(tool_input: &serde_json::Value) -> Option<Vec<String>> {
        let mut paths = Vec::new();

        match tool_input {
            serde_json::Value::Object(_) => {
                for key in [
                    "file_path",
                    "target_file",
                    "relative_workspace_path",
                    "path",
                ] {
                    if let Some(path) = tool_input.get(key).and_then(|v| v.as_str()) {
                        Self::push_unique_path(&mut paths, path);
                    }
                }

                if let Some(patch) = tool_input.get("patch").and_then(|v| v.as_str()) {
                    for path in Self::extract_patch_paths(patch) {
                        Self::push_unique_path(&mut paths, &path);
                    }
                }
            }
            serde_json::Value::String(raw_patch) => {
                for path in Self::extract_patch_paths(raw_patch) {
                    Self::push_unique_path(&mut paths, &path);
                }
            }
            _ => {}
        }

        if paths.is_empty() { None } else { Some(paths) }
    }
}

impl AgentPreset for FirebenderPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let hook_input: FirebenderHookInput = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let FirebenderHookInput {
            hook_event_name,
            model,
            repo_working_dir,
            workspace_roots,
            tool_name,
            tool_input,
            completion_id,
            dirty_files,
        } = hook_input;

        // Legacy events that should be silently skipped
        if hook_event_name == "beforeSubmitPrompt" || hook_event_name == "afterFileEdit" {
            return Err(GitAiError::PresetError(format!(
                "Skipping legacy Firebender event: {}",
                hook_event_name
            )));
        }

        if hook_event_name != "preToolUse" && hook_event_name != "postToolUse" {
            return Err(GitAiError::PresetError(format!(
                "Invalid hook_event_name: {}. Expected 'preToolUse' or 'postToolUse'",
                hook_event_name
            )));
        }

        let tool_name = tool_name.unwrap_or_default();
        let tool_class = bash_tool::classify_tool(Agent::Firebender, tool_name.as_str());
        if tool_class == ToolClass::Skip {
            return Err(GitAiError::PresetError(format!(
                "Skipping unsupported Firebender tool: {}",
                tool_name
            )));
        }
        let is_bash = tool_class == ToolClass::Bash;

        let cwd = repo_working_dir
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| workspace_roots.and_then(|roots| roots.into_iter().next()));

        let cwd_str = cwd.as_deref().unwrap_or(".");

        let tool_input = tool_input.unwrap_or(serde_json::Value::Null);
        let file_paths_raw = Self::extract_file_paths(&tool_input).map(|paths| {
            paths
                .into_iter()
                .filter_map(|path| Self::normalize_hook_path(&path, cwd_str))
                .collect::<Vec<String>>()
        });

        let file_paths: Vec<PathBuf> = file_paths_raw
            .unwrap_or_default()
            .into_iter()
            .map(|p| parse::resolve_absolute(&p, cwd_str))
            .collect();

        let model = {
            let m = model.trim().to_string();
            if m.is_empty() {
                "unknown".to_string()
            } else {
                m
            }
        };

        let session_id = completion_id.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis().to_string())
                .unwrap_or_else(|_| "0".to_string())
        });

        let dirty = dirty_files.map(|df| {
            df.into_iter()
                .map(|(k, v)| (PathBuf::from(k), v))
                .collect()
        });

        let context = PresetContext {
            agent_id: AgentId {
                tool: "firebender".to_string(),
                id: format!("firebender-{}", session_id),
                model,
            },
            session_id: session_id.clone(),
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd_str),
            metadata: HashMap::new(),
        };

        let event = match (hook_event_name.as_str(), is_bash) {
            ("preToolUse", true) => ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id: "bash".to_string(),
                strategy: BashPreHookStrategy::EmitHumanCheckpoint,
            }),
            ("preToolUse", false) => ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths,
                dirty_files: dirty,
            }),
            (_, true) => ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id: "bash".to_string(),
                transcript_source: None,
            }),
            (_, false) => ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths,
                dirty_files: dirty,
                transcript_source: None,
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
    fn test_firebender_pre_file_edit() {
        let input = json!({
            "hook_event_name": "preToolUse",
            "model": "claude-sonnet-4-5",
            "repo_working_dir": "/home/user/project",
            "tool_name": "Edit",
            "tool_input": {"file_path": "/home/user/project/src/main.rs"},
            "completion_id": "comp-123"
        })
        .to_string();
        let events = FirebenderPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "firebender");
                assert_eq!(e.context.agent_id.id, "firebender-comp-123");
                assert_eq!(e.context.agent_id.model, "claude-sonnet-4-5");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                // Path gets normalized relative to cwd
                assert_eq!(e.file_paths.len(), 1);
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_firebender_post_file_edit() {
        let input = json!({
            "hook_event_name": "postToolUse",
            "model": "claude-sonnet-4-5",
            "repo_working_dir": "/home/user/project",
            "tool_name": "Write",
            "tool_input": {"file_path": "src/lib.rs"},
            "completion_id": "comp-456"
        })
        .to_string();
        let events = FirebenderPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "firebender");
                assert!(e.transcript_source.is_none());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_firebender_pre_bash_call() {
        let input = json!({
            "hook_event_name": "preToolUse",
            "model": "claude-sonnet-4-5",
            "repo_working_dir": "/home/user/project",
            "tool_name": "Bash",
            "completion_id": "comp-789"
        })
        .to_string();
        let events = FirebenderPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "firebender");
                assert_eq!(e.tool_use_id, "bash");
                assert_eq!(e.strategy, BashPreHookStrategy::EmitHumanCheckpoint);
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_firebender_post_bash_call() {
        let input = json!({
            "hook_event_name": "postToolUse",
            "model": "claude-sonnet-4-5",
            "repo_working_dir": "/home/user/project",
            "tool_name": "Bash",
            "completion_id": "comp-789"
        })
        .to_string();
        let events = FirebenderPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "firebender");
                assert_eq!(e.tool_use_id, "bash");
                assert!(e.transcript_source.is_none());
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_firebender_skips_unsupported_tool() {
        let input = json!({
            "hook_event_name": "preToolUse",
            "model": "claude-sonnet-4-5",
            "repo_working_dir": "/home/user/project",
            "tool_name": "Search"
        })
        .to_string();
        let result = FirebenderPreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Skipping"));
    }

    #[test]
    fn test_firebender_skips_legacy_events() {
        for event in ["beforeSubmitPrompt", "afterFileEdit"] {
            let input = json!({
                "hook_event_name": event,
                "model": "claude-sonnet-4-5"
            })
            .to_string();
            let result = FirebenderPreset.parse(&input, "t_test");
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_firebender_rejects_invalid_event() {
        let input = json!({
            "hook_event_name": "unknownEvent",
            "model": "claude-sonnet-4-5"
        })
        .to_string();
        let result = FirebenderPreset.parse(&input, "t_test");
        assert!(result.is_err());
    }

    #[test]
    fn test_firebender_uses_workspace_roots_fallback() {
        let input = json!({
            "hook_event_name": "preToolUse",
            "model": "claude-sonnet-4-5",
            "workspace_roots": ["/home/user/project"],
            "tool_name": "Edit",
            "tool_input": {"file_path": "src/main.rs"},
            "completion_id": "comp-123"
        })
        .to_string();
        let events = FirebenderPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_firebender_extract_patch_paths() {
        let patch = "*** Update File: src/main.rs\n@@ -1,3 +1,4 @@\n+new line\n*** Add File: src/new.rs\n+content";
        let paths = FirebenderPreset::extract_patch_paths(patch);
        assert_eq!(paths, vec!["src/main.rs", "src/new.rs"]);
    }

    #[test]
    fn test_firebender_extract_file_paths_from_object() {
        let tool_input = json!({
            "file_path": "src/main.rs",
            "target_file": "src/lib.rs"
        });
        let paths = FirebenderPreset::extract_file_paths(&tool_input).unwrap();
        assert_eq!(paths, vec!["src/main.rs", "src/lib.rs"]);
    }

    #[test]
    fn test_firebender_extract_file_paths_from_string_patch() {
        let tool_input =
            serde_json::Value::String("*** Update File: src/main.rs\n@@ content".to_string());
        let paths = FirebenderPreset::extract_file_paths(&tool_input).unwrap();
        assert_eq!(paths, vec!["src/main.rs"]);
    }

    #[test]
    fn test_firebender_normalize_hook_path() {
        assert_eq!(
            FirebenderPreset::normalize_hook_path("/home/user/project/src/main.rs", "/home/user/project"),
            Some("src/main.rs".to_string())
        );
        assert_eq!(
            FirebenderPreset::normalize_hook_path("src/main.rs", "/home/user/project"),
            Some("src/main.rs".to_string())
        );
        assert_eq!(FirebenderPreset::normalize_hook_path("", "/home/user"), None);
    }

    #[test]
    fn test_firebender_dirty_files() {
        let input = json!({
            "hook_event_name": "preToolUse",
            "model": "claude-sonnet-4-5",
            "repo_working_dir": "/home/user/project",
            "tool_name": "Edit",
            "tool_input": {"file_path": "src/main.rs"},
            "completion_id": "comp-123",
            "dirty_files": {
                "/home/user/project/src/main.rs": "old content"
            }
        })
        .to_string();
        let events = FirebenderPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert!(e.dirty_files.is_some());
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }
}
