use super::{AgentPreset, ParsedHookEvent, PostFileEdit, PresetContext};
use crate::authorship::working_log::AgentId;
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct MockAiPreset;

impl AgentPreset for MockAiPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let mock_agent_id = format!(
            "ai-thread-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );

        let (file_paths, cwd, dirty_files) = if hook_input.is_empty() {
            (
                vec![],
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                None,
            )
        } else {
            let data: serde_json::Value = serde_json::from_str(hook_input)
                .map_err(|e| GitAiError::PresetError(format!("Invalid JSON: {}", e)))?;

            let paths: Vec<PathBuf> = data
                .get("file_paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(PathBuf::from))
                        .collect()
                })
                .unwrap_or_default();

            let cwd = data
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

            let dirty_files = super::parse::dirty_files_from_value(&data, cwd.to_str().unwrap_or("."));

            (paths, cwd, dirty_files)
        };

        let context = PresetContext {
            agent_id: AgentId {
                tool: "mock_ai".to_string(),
                id: mock_agent_id,
                model: "unknown".to_string(),
            },
            external_session_id: "mock_ai_session".to_string(),
            trace_id: trace_id.to_string(),
            cwd,
            metadata: HashMap::new(),
        };

        Ok(vec![ParsedHookEvent::PostFileEdit(PostFileEdit {
            context,
            file_paths,
            dirty_files,
            transcript_source: None,
            tool_use_id: None,
        })])
    }
}
