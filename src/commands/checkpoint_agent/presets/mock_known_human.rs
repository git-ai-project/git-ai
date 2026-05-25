use super::{AgentPreset, KnownHumanEdit, ParsedHookEvent};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct MockKnownHumanPreset;

impl AgentPreset for MockKnownHumanPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let (file_paths, cwd, dirty_files) = if hook_input.is_empty() {
            (
                vec![],
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                None,
            )
        } else {
            let data: serde_json::Value = serde_json::from_str(hook_input)
                .map_err(|e| GitAiError::PresetError(format!("Invalid JSON: {}", e)))?;

            let paths = data
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

        Ok(vec![ParsedHookEvent::KnownHumanEdit(KnownHumanEdit {
            trace_id: trace_id.to_string(),
            cwd,
            file_paths,
            dirty_files,
            editor_metadata: HashMap::new(),
        })])
    }
}
