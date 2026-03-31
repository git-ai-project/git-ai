use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{
    binary_exists, generate_diff, home_dir, is_git_ai_checkpoint_command, write_atomic,
};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

const WINDSURF_CHECKPOINT_CMD: &str = "checkpoint windsurf --hook-input stdin";

/// The three Windsurf Cascade hook events we install into.
const HOOK_EVENTS: &[&str] = &[
    "pre_write_code",
    "post_write_code",
    "post_cascade_response_with_transcript",
];

pub struct WindsurfInstaller;

impl WindsurfInstaller {
    /// Both locations where Windsurf looks for hooks.
    fn hooks_paths() -> [PathBuf; 2] {
        // https://docs.windsurf.com/windsurf/cascade/hooks#user-level
        let codeium = home_dir().join(".codeium");
        [
            // for intellej
            codeium.join("hooks.json"),
            // for windsurf
            codeium.join("windsurf").join("hooks.json"),
        ]
    }

    /// Install hooks into a single hooks.json file, returning a diff if changes were made.
    fn install_hooks_at(
        hooks_path: &PathBuf,
        desired_cmd: &str,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        if let Some(dir) = hooks_path.parent() {
            fs::create_dir_all(dir)?;
        }

        let existing_content = if hooks_path.exists() {
            fs::read_to_string(hooks_path)?
        } else {
            String::new()
        };

        let existing: Value = if existing_content.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&existing_content)?
        };

        let mut merged = existing.clone();
        let mut hooks_obj = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));

        for event in HOOK_EVENTS {
            let mut event_array = hooks_obj
                .get(*event)
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Find existing git-ai command
            let mut found_idx: Option<usize> = None;
            let mut needs_update = false;

            for (idx, item) in event_array.iter().enumerate() {
                if let Some(cmd) = item.get("command").and_then(|c| c.as_str())
                    && is_git_ai_checkpoint_command(cmd)
                    && found_idx.is_none()
                {
                    found_idx = Some(idx);
                    if cmd != desired_cmd {
                        needs_update = true;
                    }
                }
            }

            match found_idx {
                Some(idx) => {
                    if needs_update {
                        event_array[idx] = json!({
                            "command": desired_cmd,
                            "show_output": false
                        });
                    }
                    // Remove duplicates
                    let keep_idx = idx;
                    let mut current_idx = 0;
                    event_array.retain(|item| {
                        if current_idx == keep_idx {
                            current_idx += 1;
                            true
                        } else if let Some(cmd) = item.get("command").and_then(|c| c.as_str()) {
                            let is_dup = is_git_ai_checkpoint_command(cmd);
                            current_idx += 1;
                            !is_dup
                        } else {
                            current_idx += 1;
                            true
                        }
                    });
                }
                None => {
                    event_array.push(json!({
                        "command": desired_cmd,
                        "show_output": false
                    }));
                }
            }

            if let Some(obj) = hooks_obj.as_object_mut() {
                obj.insert(event.to_string(), Value::Array(event_array));
            }
        }

        if let Some(root) = merged.as_object_mut() {
            root.insert("hooks".to_string(), hooks_obj);
        }

        if existing == merged {
            return Ok(None);
        }

        let new_content = serde_json::to_string_pretty(&merged)?;
        let diff_output = generate_diff(hooks_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(hooks_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }

    /// Remove hooks from a single hooks.json file, returning a diff if changes were made.
    fn uninstall_hooks_at(
        hooks_path: &PathBuf,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        if !hooks_path.exists() {
            return Ok(None);
        }

        let existing_content = fs::read_to_string(hooks_path)?;
        let existing: Value = serde_json::from_str(&existing_content)?;

        let mut merged = existing.clone();
        let mut hooks_obj = match merged.get("hooks").cloned() {
            Some(h) => h,
            None => return Ok(None),
        };

        let mut changed = false;

        for event in HOOK_EVENTS {
            if let Some(event_array) = hooks_obj.get_mut(*event).and_then(|v| v.as_array_mut()) {
                let original_len = event_array.len();
                event_array.retain(|item| {
                    if let Some(cmd) = item.get("command").and_then(|c| c.as_str()) {
                        !is_git_ai_checkpoint_command(cmd)
                    } else {
                        true
                    }
                });
                if event_array.len() != original_len {
                    changed = true;
                }
            }
        }

        if !changed {
            return Ok(None);
        }

        if let Some(root) = merged.as_object_mut() {
            root.insert("hooks".to_string(), hooks_obj);
        }

        let new_content = serde_json::to_string_pretty(&merged)?;
        let diff_output = generate_diff(hooks_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(hooks_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }
}

impl HookInstaller for WindsurfInstaller {
    fn name(&self) -> &str {
        "Windsurf"
    }

    fn id(&self) -> &str {
        "windsurf"
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary = binary_exists("windsurf");
        let has_dotfiles = home_dir().join(".codeium").join("windsurf").exists();

        if !has_binary && !has_dotfiles {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        // Check all hook locations
        let mut any_installed = false;
        let mut all_installed = true;
        for hooks_path in Self::hooks_paths() {
            if !hooks_path.exists() {
                all_installed = false;
                continue;
            }

            let content = fs::read_to_string(&hooks_path)?;
            let existing: Value = serde_json::from_str(&content).unwrap_or_else(|_| json!({}));

            let has_hooks = HOOK_EVENTS.iter().all(|event| {
                existing
                    .get("hooks")
                    .and_then(|h| h.get(*event))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter().any(|item| {
                            item.get("command")
                                .and_then(|c| c.as_str())
                                .map(is_git_ai_checkpoint_command)
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            });

            if has_hooks {
                any_installed = true;
            } else {
                all_installed = false;
            }
        }

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed: any_installed,
            hooks_up_to_date: all_installed,
        })
    }

    fn install_hooks(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let desired_cmd = format!(
            "{} {}",
            params.binary_path.display(),
            WINDSURF_CHECKPOINT_CMD
        );

        let mut all_diffs = Vec::new();

        for hooks_path in Self::hooks_paths() {
            if let Some(diff) = Self::install_hooks_at(&hooks_path, &desired_cmd, dry_run)? {
                all_diffs.push(diff);
            }
        }

        if all_diffs.is_empty() {
            Ok(None)
        } else {
            Ok(Some(all_diffs.join("\n")))
        }
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let mut all_diffs = Vec::new();

        for hooks_path in Self::hooks_paths() {
            if let Some(diff) = Self::uninstall_hooks_at(&hooks_path, dry_run)? {
                all_diffs.push(diff);
            }
        }

        if all_diffs.is_empty() {
            Ok(None)
        } else {
            Ok(Some(all_diffs.join("\n")))
        }
    }
}
