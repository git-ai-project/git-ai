use super::super::parse;
use super::super::{
    ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit, PresetContext,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::ToolClass;
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub(super) fn parse_cli_hooks(
    data: &serde_json::Value,
    hook_event_name: &str,
    trace_id: &str,
) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    let cwd = parse::optional_str_multi(data, &["cwd", "workspace_folder", "workspaceFolder"])
        .ok_or_else(|| GitAiError::PresetError("cwd not found in hook_input".to_string()))?;

    let session_id = super::extract_session_id(data);
    let tool_name =
        parse::optional_str_multi(data, &["tool_name", "toolName"]).unwrap_or("unknown");

    let class = classify_cli_tool(tool_name);
    if class == ToolClass::Skip {
        return Err(GitAiError::PresetError(format!(
            "Skipping CopilotCLI hook for non-edit tool '{}'.",
            tool_name
        )));
    }

    let dirty_files = super::dirty_files_from_hook_data(data, cwd);

    let tool_input = data.get("tool_input").or_else(|| data.get("toolInput"));
    let tool_result = data
        .get("tool_result")
        .or_else(|| data.get("toolResult"))
        .or_else(|| data.get("tool_response"));

    let extracted_paths =
        super::extract_filepaths_from_vscode_hook_payload(tool_input, tool_result, cwd);

    // tool_use_id is absent in CopilotCLI payloads; synthesize a stable id from session+tool_name.
    // CLI bash invocations are sync (one in flight per session) so this id is enough for Pre/Post
    // pairing within the same session.
    let tool_use_id = parse::optional_str_multi(data, &["tool_use_id", "toolUseId"])
        .map(str::to_string)
        .unwrap_or_else(|| format!("cli-{}-{}", session_id, tool_name));

    let mut metadata = HashMap::new();
    metadata.insert("source".to_string(), "copilot-cli".to_string());

    let context = PresetContext {
        agent_id: AgentId {
            tool: "github-copilot".to_string(),
            id: session_id.clone(),
            model: "unknown".to_string(),
        },
        session_id,
        trace_id: trace_id.to_string(),
        cwd: PathBuf::from(cwd),
        metadata,
    };

    match (hook_event_name, class) {
        ("PreToolUse", ToolClass::Bash) => Ok(vec![ParsedHookEvent::PreBashCall(PreBashCall {
            context,
            tool_use_id,
        })]),
        ("PostToolUse", ToolClass::Bash) => Ok(vec![ParsedHookEvent::PostBashCall(PostBashCall {
            context,
            tool_use_id,
            transcript_source: None,
        })]),
        ("PreToolUse", ToolClass::FileEdit) => {
            // `create` PreToolUse: synthesize empty dirty_files for the new path
            // (mirrors the IDE `create_file` behavior).
            if tool_name == "create" {
                if extracted_paths.is_empty() {
                    return Err(GitAiError::PresetError(
                        "No path in CopilotCLI create tool_input".to_string(),
                    ));
                }
                let dirty_files: HashMap<PathBuf, String> = extracted_paths
                    .iter()
                    .map(|p| (p.clone(), String::new()))
                    .collect();
                return Ok(vec![ParsedHookEvent::PreFileEdit(PreFileEdit {
                    context,
                    file_paths: extracted_paths,
                    dirty_files: Some(dirty_files),
                    tool_use_id: Some(tool_use_id),
                })]);
            }
            if extracted_paths.is_empty() {
                return Err(GitAiError::PresetError(format!(
                    "No file paths in CopilotCLI {} PreToolUse tool_input",
                    tool_name
                )));
            }
            Ok(vec![ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths: extracted_paths,
                dirty_files,
                tool_use_id: Some(tool_use_id),
            })])
        }
        ("PostToolUse", ToolClass::FileEdit) => {
            if extracted_paths.is_empty() {
                return Err(GitAiError::PresetError(format!(
                    "No file paths in CopilotCLI {} PostToolUse tool_input",
                    tool_name
                )));
            }
            Ok(vec![ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths: extracted_paths,
                dirty_files,
                transcript_source: None,
                tool_use_id: Some(tool_use_id),
            })])
        }
        _ => unreachable!("hook_event_name pre-validated by mod.rs fork"),
    }
}

fn classify_cli_tool(tool: &str) -> ToolClass {
    match tool {
        "bash" => ToolClass::Bash,
        "create" | "str_replace" => ToolClass::FileEdit,
        // Skip:
        //   report_intent — intent logging only, no file changes.
        //   read_bash / write_bash / stop_bash — control ops on an already-running async shell;
        //   the originating `bash` Pre/Post brackets the file changes.
        _ => ToolClass::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::AgentPreset;
    use super::super::GithubCopilotPreset;
    use super::*;
    use serde_json::json;

    #[test]
    fn cli_bash_pre() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "sess-cli",
            "cwd": "/Users/a/project",
            "tool_name": "bash",
            "tool_input": {
                "command": "cd /Users/a/project && cat > new.txt",
                "description": "Create file",
                "mode": "sync",
                "initial_wait": 30
            }
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "github-copilot");
                assert_eq!(
                    e.context.metadata.get("source"),
                    Some(&"copilot-cli".to_string())
                );
                assert_eq!(e.tool_use_id, "cli-sess-cli-bash");
            }
            other => panic!("Expected PreBashCall, got {:?}", other),
        }
    }

    #[test]
    fn cli_bash_post() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-cli",
            "cwd": "/Users/a/project",
            "tool_name": "bash",
            "tool_input": {"command": "ls", "description": "list", "mode": "sync", "initial_wait": 30},
            "tool_result": {"result_type": "success", "text_result_for_llm": ""}
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert!(e.transcript_source.is_none());
                assert_eq!(e.tool_use_id, "cli-sess-cli-bash");
            }
            other => panic!("Expected PostBashCall, got {:?}", other),
        }
    }

    #[test]
    fn cli_create_pre_synthesizes_empty_dirty_files() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "sess-cli",
            "cwd": "/Users/a/project",
            "tool_name": "create",
            "tool_input": {
                "path": "/Users/a/project/very_fun.md",
                "file_text": "# heading\n"
            }
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/Users/a/project/very_fun.md")]
                );
                assert_eq!(
                    e.dirty_files
                        .as_ref()
                        .unwrap()
                        .get(&PathBuf::from("/Users/a/project/very_fun.md")),
                    Some(&String::new())
                );
            }
            other => panic!("Expected PreFileEdit, got {:?}", other),
        }
    }

    #[test]
    fn cli_create_post() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-cli",
            "cwd": "/Users/a/project",
            "tool_name": "create",
            "tool_input": {
                "path": "/Users/a/project/very_fun.md",
                "file_text": "# heading\n"
            },
            "tool_result": {"result_type": "success", "text_result_for_llm": "Created file"}
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/Users/a/project/very_fun.md")]
                );
                assert!(e.transcript_source.is_none());
            }
            other => panic!("Expected PostFileEdit, got {:?}", other),
        }
    }

    #[test]
    fn cli_str_replace_pre_post() {
        let pre = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "sess-cli",
            "cwd": "/Users/a/project",
            "tool_name": "str_replace",
            "tool_input": {
                "path": "/Users/a/project/fun.md",
                "old_str": "hello",
                "new_str": "world"
            }
        })
        .to_string();
        let pre_events = GithubCopilotPreset.parse(&pre, "t_test123456789a").unwrap();
        assert!(matches!(pre_events[0], ParsedHookEvent::PreFileEdit(_)));

        let post = json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-cli",
            "cwd": "/Users/a/project",
            "tool_name": "str_replace",
            "tool_input": {
                "path": "/Users/a/project/fun.md",
                "old_str": "hello",
                "new_str": "world"
            },
            "tool_result": {"result_type": "success", "text_result_for_llm": ""}
        })
        .to_string();
        let post_events = GithubCopilotPreset
            .parse(&post, "t_test123456789a")
            .unwrap();
        assert!(matches!(post_events[0], ParsedHookEvent::PostFileEdit(_)));
    }

    #[test]
    fn cli_relative_path_resolved_against_cwd() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "sess-cli",
            "cwd": "/Users/a/project",
            "tool_name": "create",
            "tool_input": {"path": "subdir/relative.md", "file_text": "x"}
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/Users/a/project/subdir/relative.md")]
                );
            }
            other => panic!("Expected PreFileEdit, got {:?}", other),
        }
    }

    #[test]
    fn cli_skips_report_intent() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "sess-cli",
            "cwd": "/Users/a/project",
            "tool_name": "report_intent",
            "tool_input": {"intent": "Creating file"}
        })
        .to_string();
        let result = GithubCopilotPreset.parse(&input, "t_test123456789a");
        assert!(result.is_err());
    }

    #[test]
    fn cli_skips_read_bash_write_bash_stop_bash() {
        for tool in &["read_bash", "write_bash", "stop_bash"] {
            let input = json!({
                "hook_event_name": "PreToolUse",
                "session_id": "sess-cli",
                "cwd": "/Users/a/project",
                "tool_name": tool,
                "tool_input": {"shellId": "0"}
            })
            .to_string();
            let result = GithubCopilotPreset.parse(&input, "t_test123456789a");
            assert!(
                result.is_err(),
                "Expected {} PreToolUse to be skipped",
                tool
            );
        }
    }

    #[test]
    fn cli_create_with_no_path_errors() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "sess-cli",
            "cwd": "/Users/a/project",
            "tool_name": "create",
            "tool_input": {"file_text": "no path here"}
        })
        .to_string();
        let result = GithubCopilotPreset.parse(&input, "t_test123456789a");
        assert!(result.is_err());
    }

    #[test]
    fn classify_cli_tool_matrix() {
        assert_eq!(classify_cli_tool("bash"), ToolClass::Bash);
        assert_eq!(classify_cli_tool("create"), ToolClass::FileEdit);
        assert_eq!(classify_cli_tool("str_replace"), ToolClass::FileEdit);
        assert_eq!(classify_cli_tool("report_intent"), ToolClass::Skip);
        assert_eq!(classify_cli_tool("read_bash"), ToolClass::Skip);
        assert_eq!(classify_cli_tool("write_bash"), ToolClass::Skip);
        assert_eq!(classify_cli_tool("stop_bash"), ToolClass::Skip);
        assert_eq!(classify_cli_tool("nonsense"), ToolClass::Skip);
    }
}
