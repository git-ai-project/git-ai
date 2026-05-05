use super::opencode::{OpenCodeFamilyConfig, parse_opencode_family};
use super::{AgentPreset, ParsedHookEvent};
use crate::commands::checkpoint_agent::bash_tool::Agent;
use crate::error::GitAiError;

pub struct KiloPreset;

pub(crate) static KILO_CONFIG: OpenCodeFamilyConfig = OpenCodeFamilyConfig {
    tool_name: "kilo",
    db_filename: "kilo.db",
    data_dir_name: "kilo",
    storage_env_var: "GIT_AI_KILO_STORAGE_PATH",
    bash_agent: Agent::Kilo,
};

impl AgentPreset for KiloPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        parse_opencode_family(&KILO_CONFIG, hook_input, trace_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::checkpoint_agent::presets::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn make_kilo_input(event: &str, tool: &str) -> String {
        json!({
            "hook_event_name": event,
            "session_id": "kilo-sess-123",
            "cwd": "/home/user/project",
            "tool_name": tool,
            "tool_use_id": "tu-1",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string()
    }

    #[test]
    fn test_kilo_pre_file_edit() {
        let input = make_kilo_input("PreToolUse", "edit");
        let events = KiloPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "kilo");
                assert_eq!(e.context.external_session_id, "kilo-sess-123");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert!(!e.file_paths.is_empty());
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_kilo_post_file_edit() {
        let input = make_kilo_input("PostToolUse", "write");
        let events = KiloPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "kilo");
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_kilo_pre_bash_call() {
        let input = make_kilo_input("PreToolUse", "bash");
        let events = KiloPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "kilo");
                assert_eq!(e.tool_use_id, "tu-1");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_kilo_post_bash_call() {
        let input = make_kilo_input("PostToolUse", "shell");
        let events = KiloPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "kilo");
                assert_eq!(e.tool_use_id, "tu-1");
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_kilo_extracts_file_paths_from_tool_input() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-1",
            "cwd": "/project",
            "tool_name": "edit",
            "tool_input": {
                "file_path": "src/main.rs",
                "fspath": "/project/src/lib.rs"
            }
        })
        .to_string();
        let events = KiloPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert!(!e.file_paths.is_empty());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_kilo_default_tool_use_id() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "sess-1",
            "cwd": "/project",
            "tool_name": "bash"
        })
        .to_string();
        let events = KiloPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.tool_use_id, "bash");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }
}
