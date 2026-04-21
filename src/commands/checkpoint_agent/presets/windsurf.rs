use super::parse;
use super::{
    AgentPreset, BashPreHookStrategy, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall,
    PreFileEdit, PresetContext, TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct WindsurfPreset;

impl AgentPreset for WindsurfPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let trajectory_id = parse::required_str(&data, "trajectory_id")?.to_string();
        let agent_action = parse::optional_str(&data, "agent_action_name");

        let tool_info = data.get("tool_info");
        let cwd = tool_info
            .and_then(|ti| ti.get("cwd"))
            .and_then(|v| v.as_str())
            .or_else(|| parse::optional_str(&data, "cwd"));

        let model = parse::optional_str(&data, "model_name")
            .filter(|s| !s.is_empty() && *s != "Unknown")
            .unwrap_or("unknown")
            .to_string();

        let transcript_path = tool_info
            .and_then(|ti| ti.get("transcript_path"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
                format!(
                    "{}/.windsurf/transcripts/{}.jsonl",
                    home.display(),
                    trajectory_id
                )
            });

        // cwd is optional: prefer tool_info.cwd, fall back to top-level cwd, then
        // current working directory as last resort.
        let cwd_path = cwd
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let cwd_str = cwd_path.to_string_lossy().to_string();

        let context = PresetContext {
            agent_id: AgentId {
                tool: "windsurf".to_string(),
                id: trajectory_id.clone(),
                model,
            },
            session_id: trajectory_id,
            trace_id: trace_id.to_string(),
            cwd: cwd_path,
            metadata: HashMap::from([("transcript_path".to_string(), transcript_path.clone())]),
        };

        let transcript_source = Some(TranscriptSource::Path {
            path: PathBuf::from(&transcript_path),
            format: TranscriptFormat::WindsurfJsonl,
            session_id: None,
        });

        let is_bash = matches!(
            agent_action,
            Some("pre_run_command") | Some("post_run_command")
        );
        let is_pre_bash = matches!(agent_action, Some("pre_run_command"));
        let is_pre_write = matches!(agent_action, Some("pre_write_code"));

        let execution_id = tool_info
            .and_then(|ti| ti.get("execution_id"))
            .and_then(|v| v.as_str())
            .or_else(|| parse::optional_str(&data, "execution_id"))
            .unwrap_or("bash")
            .to_string();

        let event = if is_bash {
            if is_pre_bash {
                ParsedHookEvent::PreBashCall(PreBashCall {
                    context,
                    tool_use_id: execution_id,
                    strategy: BashPreHookStrategy::EmitHumanCheckpoint,
                })
            } else {
                ParsedHookEvent::PostBashCall(PostBashCall {
                    context,
                    tool_use_id: execution_id,
                    transcript_source,
                })
            }
        } else if is_pre_write {
            let file_path = tool_info
                .and_then(|ti| ti.get("file_path"))
                .and_then(|v| v.as_str())
                .map(|p| vec![parse::resolve_absolute(p, &cwd_str)])
                .unwrap_or_default();

            ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths: file_path,
                dirty_files: None,
            })
        } else {
            let file_path = tool_info
                .and_then(|ti| ti.get("file_path"))
                .and_then(|v| v.as_str())
                .map(|p| vec![parse::resolve_absolute(p, &cwd_str)])
                .unwrap_or_default();

            ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths: file_path,
                dirty_files: None,
                transcript_source,
            })
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
    fn test_windsurf_post_file_edit() {
        let input = json!({
            "trajectory_id": "traj-123",
            "agent_action_name": "post_code_action",
            "model_name": "gpt-4",
            "tool_info": {
                "cwd": "/home/user/project",
                "file_path": "src/main.rs",
                "transcript_path": "/home/user/.windsurf/transcripts/traj-123.jsonl"
            }
        })
        .to_string();
        let events = WindsurfPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "windsurf");
                assert_eq!(e.context.session_id, "traj-123");
                assert_eq!(e.context.agent_id.model, "gpt-4");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
                assert!(matches!(
                    e.transcript_source,
                    Some(TranscriptSource::Path {
                        format: TranscriptFormat::WindsurfJsonl,
                        ..
                    })
                ));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_windsurf_pre_bash_call() {
        let input = json!({
            "trajectory_id": "traj-123",
            "agent_action_name": "pre_run_command",
            "cwd": "/home/user/project",
            "execution_id": "exec-bash-1"
        })
        .to_string();
        let events = WindsurfPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "windsurf");
                assert_eq!(e.tool_use_id, "exec-bash-1");
                assert_eq!(e.strategy, BashPreHookStrategy::EmitHumanCheckpoint);
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_windsurf_post_bash_call() {
        let input = json!({
            "trajectory_id": "traj-123",
            "agent_action_name": "post_run_command",
            "cwd": "/home/user/project",
            "execution_id": "exec-bash-2"
        })
        .to_string();
        let events = WindsurfPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "windsurf");
                assert_eq!(e.tool_use_id, "exec-bash-2");
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_windsurf_bash_fallback_tool_use_id() {
        let input = json!({
            "trajectory_id": "traj-123",
            "agent_action_name": "pre_run_command",
            "cwd": "/home/user/project"
        })
        .to_string();
        let events = WindsurfPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.tool_use_id, "bash");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_windsurf_cwd_from_tool_info() {
        let input = json!({
            "trajectory_id": "traj-123",
            "agent_action_name": "post_code_action",
            "cwd": "/fallback/path",
            "tool_info": {
                "cwd": "/preferred/path",
                "file_path": "src/main.rs"
            }
        })
        .to_string();
        let events = WindsurfPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.cwd, PathBuf::from("/preferred/path"));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_windsurf_cwd_fallback_to_top_level() {
        let input = json!({
            "trajectory_id": "traj-123",
            "agent_action_name": "post_code_action",
            "cwd": "/fallback/path"
        })
        .to_string();
        let events = WindsurfPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.cwd, PathBuf::from("/fallback/path"));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_windsurf_default_transcript_path() {
        let input = json!({
            "trajectory_id": "traj-456",
            "agent_action_name": "post_code_action",
            "cwd": "/home/user/project"
        })
        .to_string();
        let events = WindsurfPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                let tp = e.context.metadata.get("transcript_path").unwrap();
                assert!(tp.contains(".windsurf/transcripts/traj-456.jsonl"));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_windsurf_missing_cwd_falls_back() {
        let input = json!({
            "trajectory_id": "traj-123",
            "agent_action_name": "post_code_action"
        })
        .to_string();
        let events = WindsurfPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                // cwd should fall back to current_dir or "."
                assert!(!e.context.cwd.as_os_str().is_empty());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_windsurf_pre_write_code() {
        let input = json!({
            "trajectory_id": "traj-123",
            "agent_action_name": "pre_write_code",
            "cwd": "/home/user/project",
            "tool_info": {
                "file_path": "src/main.rs"
            }
        })
        .to_string();
        let events = WindsurfPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "windsurf");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }
}
