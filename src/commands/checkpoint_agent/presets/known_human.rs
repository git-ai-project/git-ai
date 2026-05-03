use super::{AgentPreset, KnownHumanEdit, ParsedHookEvent};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct KnownHumanPreset;

impl AgentPreset for KnownHumanPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let (editor, editor_version, extension_version, cwd, file_paths, dirty_files) =
            if hook_input.is_empty() {
                (
                    "unknown".to_string(),
                    "unknown".to_string(),
                    "unknown".to_string(),
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                    vec![],
                    None,
                )
            } else {
                let data: serde_json::Value = serde_json::from_str(hook_input)
                    .map_err(|e| GitAiError::PresetError(format!("Invalid JSON: {}", e)))?;

                let editor = data["editor"].as_str().unwrap_or("unknown").to_string();
                let editor_version = data["editor_version"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                let extension_version = data["extension_version"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();

                let cwd = data["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                });

                let file_paths = data["edited_filepaths"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(PathBuf::from))
                            .collect()
                    })
                    .unwrap_or_default();

                let dirty_files = data["dirty_files"].as_object().map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (PathBuf::from(k), s.to_string())))
                        .collect::<HashMap<PathBuf, String>>()
                });

                (
                    editor,
                    editor_version,
                    extension_version,
                    cwd,
                    file_paths,
                    dirty_files,
                )
            };

        let mut editor_metadata = HashMap::new();
        editor_metadata.insert("kh_editor".to_string(), editor);
        editor_metadata.insert("kh_editor_version".to_string(), editor_version);
        editor_metadata.insert("kh_extension_version".to_string(), extension_version);

        Ok(vec![ParsedHookEvent::KnownHumanEdit(KnownHumanEdit {
            trace_id: trace_id.to_string(),
            cwd,
            file_paths,
            dirty_files,
            editor_metadata,
        })])
    }
}
