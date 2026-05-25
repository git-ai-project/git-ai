use super::{AgentPreset, ParsedHookEvent, UntrackedEdit, parse};
use crate::error::GitAiError;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct JuniePreset;

impl JuniePreset {
    fn current_cwd(data: &serde_json::Value) -> PathBuf {
        parse::optional_str(data, "cwd")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    fn dirty_file_paths(cwd: &Path) -> Vec<PathBuf> {
        let repo_root = crate::git::repository::discover_repository_in_path_no_git_exec(cwd)
            .ok()
            .and_then(|repo| repo.workdir().ok())
            .unwrap_or_else(|| cwd.to_path_buf());

        let output = Command::new(crate::config::Config::get().git_cmd())
            .args(["status", "--porcelain", "-uall"])
            .current_dir(cwd)
            .output()
            .ok();

        let Some(output) = output else {
            return vec![];
        };

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                if line.len() < 4 {
                    return None;
                }
                let raw_file = line[3..].trim();
                if raw_file.is_empty() {
                    return None;
                }
                let unescaped = crate::utils::unescape_git_path(raw_file);
                let file = unescaped
                    .find(" -> ")
                    .map(|arrow| &unescaped[arrow + 4..])
                    .unwrap_or(unescaped.as_str());
                let path = Path::new(file);
                if path.is_absolute() {
                    Some(path.to_path_buf())
                } else {
                    Some(repo_root.join(path))
                }
            })
            .collect()
    }
}

impl AgentPreset for JuniePreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let hook_event = parse::required_str(&data, "hook_event_name")?;
        if hook_event != "SessionStart" {
            return Err(GitAiError::PresetError(format!(
                "Unsupported Junie hook_event_name: {hook_event}"
            )));
        }

        let source = parse::required_str(&data, "source")?;
        if !matches!(source, "startup" | "resume") {
            return Err(GitAiError::PresetError(format!(
                "Unsupported Junie source: {source}"
            )));
        }

        let cwd = Self::current_cwd(&data);
        let file_paths = Self::dirty_file_paths(&cwd);

        Ok(vec![ParsedHookEvent::UntrackedEdit(UntrackedEdit {
            trace_id: trace_id.to_string(),
            cwd,
            file_paths,
        })])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::checkpoint_agent::presets::ParsedHookEvent;
    use serde_json::json;

    #[test]
    fn test_junie_session_start_uses_cwd_from_payload() {
        let input = json!({
            "hook_event_name": "SessionStart",
            "source": "resume",
            "cwd": "/tmp/project"
        })
        .to_string();

        let events = JuniePreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::UntrackedEdit(edit) => {
                assert_eq!(edit.trace_id, "t_test");
                assert_eq!(edit.cwd, PathBuf::from("/tmp/project"));
            }
            _ => panic!("Expected UntrackedEdit"),
        }
    }

    #[test]
    fn test_junie_rejects_non_session_start_events() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "source": "startup"
        })
        .to_string();

        let err = JuniePreset.parse(&input, "t_test").unwrap_err();
        assert!(
            err.to_string()
                .contains("Unsupported Junie hook_event_name")
        );
    }
}
