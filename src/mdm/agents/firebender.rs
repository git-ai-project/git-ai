use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{generate_diff, home_dir, write_atomic};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

const FIREBENDER_CHECKPOINT_CMD: &str = "checkpoint firebender --hook-input stdin";

pub struct FirebenderInstaller;

impl FirebenderInstaller {
    fn hooks_path() -> PathBuf {
        home_dir().join(".firebender").join("hooks.json")
    }

    fn is_firebender_checkpoint_command(cmd: &str) -> bool {
        cmd.contains("git-ai checkpoint firebender")
            || (cmd.contains("git-ai")
                && cmd.contains("checkpoint")
                && cmd.contains("firebender"))
    }
}

impl HookInstaller for FirebenderInstaller {
    fn name(&self) -> &str {
        "Firebender"
    }

    fn id(&self) -> &str {
        "firebender"
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_dotfiles = home_dir().join(".firebender").exists();
        if !has_dotfiles {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let hooks_path = Self::hooks_path();
        if !hooks_path.exists() {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let content = fs::read_to_string(&hooks_path)?;
        let existing: Value = serde_json::from_str(&content).unwrap_or_else(|_| json!({}));

        let has_before = existing
            .get("hooks")
            .and_then(|h| h.get("beforeSubmitPrompt"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|item| {
                    item.get("command")
                        .and_then(|c| c.as_str())
                        .map(Self::is_firebender_checkpoint_command)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        let has_after = existing
            .get("hooks")
            .and_then(|h| h.get("afterFileEdit"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|item| {
                    item.get("command")
                        .and_then(|c| c.as_str())
                        .map(Self::is_firebender_checkpoint_command)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed: has_before && has_after,
            hooks_up_to_date: has_before && has_after,
        })
    }

    fn install_hooks(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let hooks_path = Self::hooks_path();
        if let Some(dir) = hooks_path.parent() {
            fs::create_dir_all(dir)?;
        }

        let existing_content = if hooks_path.exists() {
            fs::read_to_string(&hooks_path)?
        } else {
            String::new()
        };

        let existing: Value = if existing_content.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&existing_content)?
        };

        let command = format!(
            "{} {}",
            params.binary_path.display(),
            FIREBENDER_CHECKPOINT_CMD
        );

        let desired: Value = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    {
                        "command": command
                    }
                ],
                "afterFileEdit": [
                    {
                        "command": command
                    }
                ]
            }
        });

        let mut merged = existing.clone();
        if merged.get("version").is_none() && let Some(obj) = merged.as_object_mut() {
            obj.insert("version".to_string(), json!(1));
        }

        let mut hooks_obj = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));

        for hook_name in &["beforeSubmitPrompt", "afterFileEdit"] {
            let desired_hooks = desired
                .get("hooks")
                .and_then(|h| h.get(*hook_name))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let mut existing_hooks = hooks_obj
                .get(*hook_name)
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            for desired_hook in desired_hooks {
                let Some(desired_cmd) = desired_hook.get("command").and_then(|c| c.as_str()) else {
                    continue;
                };

                let mut found_idx = None;
                let mut needs_update = false;

                for (idx, existing_hook) in existing_hooks.iter().enumerate() {
                    if let Some(existing_cmd) = existing_hook.get("command").and_then(|c| c.as_str())
                        && Self::is_firebender_checkpoint_command(existing_cmd)
                    {
                        found_idx = Some(idx);
                        if existing_cmd != desired_cmd {
                            needs_update = true;
                        }
                        break;
                    }
                }

                match found_idx {
                    Some(idx) if needs_update => existing_hooks[idx] = desired_hook.clone(),
                    Some(_) => {}
                    None => existing_hooks.push(desired_hook.clone()),
                }
            }

            if let Some(obj) = hooks_obj.as_object_mut() {
                obj.insert(hook_name.to_string(), Value::Array(existing_hooks));
            }
        }

        if let Some(root) = merged.as_object_mut() {
            root.insert("hooks".to_string(), hooks_obj);
        }

        if existing == merged {
            return Ok(None);
        }

        let new_content = serde_json::to_string_pretty(&merged)?;
        let diff_output = generate_diff(&hooks_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(&hooks_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let hooks_path = Self::hooks_path();
        if !hooks_path.exists() {
            return Ok(None);
        }

        let existing_content = fs::read_to_string(&hooks_path)?;
        let existing: Value = serde_json::from_str(&existing_content)?;

        let mut merged = existing.clone();
        let mut hooks_obj = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));
        let mut changed = false;

        for hook_name in &["beforeSubmitPrompt", "afterFileEdit"] {
            if let Some(arr) = hooks_obj.get_mut(*hook_name).and_then(|v| v.as_array_mut()) {
                let original_len = arr.len();
                arr.retain(|item| {
                    if let Some(cmd) = item.get("command").and_then(|c| c.as_str()) {
                        !Self::is_firebender_checkpoint_command(cmd)
                    } else {
                        true
                    }
                });
                if arr.len() != original_len {
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
        let diff_output = generate_diff(&hooks_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(&hooks_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }
}
