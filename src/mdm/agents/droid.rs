use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{generate_diff, home_dir, is_git_ai_checkpoint_command, write_atomic};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

const DROID_PRE_TOOL_CMD: &str = "checkpoint droid --hook-input stdin";
const DROID_POST_TOOL_CMD: &str = "checkpoint droid --hook-input stdin";

pub struct DroidInstaller;

impl DroidInstaller {
    fn settings_path() -> PathBuf {
        home_dir().join(".factory").join("settings.json")
    }
}

impl HookInstaller for DroidInstaller {
    fn name(&self) -> &str {
        "Droid"
    }

    fn id(&self) -> &str {
        "droid"
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_dotfiles = home_dir().join(".factory").exists();

        if !has_dotfiles {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let settings_path = Self::settings_path();
        if !settings_path.exists() {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let content = fs::read_to_string(&settings_path)?;
        let existing: Value = serde_json::from_str(&content).unwrap_or_else(|_| json!({}));

        let has_hooks = existing
            .get("hooks")
            .and_then(|h| h.get("PreToolUse"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|item| {
                    item.get("hooks")
                        .and_then(|h| h.as_array())
                        .map(|hooks| {
                            hooks.iter().any(|hook| {
                                hook.get("command")
                                    .and_then(|c| c.as_str())
                                    .map(is_git_ai_checkpoint_command)
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed: has_hooks,
            hooks_up_to_date: has_hooks,
        })
    }

    fn install_hooks(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let settings_path = Self::settings_path();

        if let Some(dir) = settings_path.parent() {
            fs::create_dir_all(dir)?;
        }

        let existing_content = if settings_path.exists() {
            fs::read_to_string(&settings_path)?
        } else {
            String::new()
        };

        let existing: Value = if existing_content.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&existing_content)?
        };

        let binary_path = params.binary_path.to_string_lossy().to_string();
        let pre_tool_cmd = format!("{} {}", binary_path, DROID_PRE_TOOL_CMD);
        let post_tool_cmd = format!("{} {}", binary_path, DROID_POST_TOOL_CMD);

        let desired_hooks = json!({
            "PreToolUse": {
                "matcher": "^(Edit|Write|Create|ApplyPatch)$",
                "desired_cmd": pre_tool_cmd,
            },
            "PostToolUse": {
                "matcher": "^(Edit|Write|Create|ApplyPatch)$",
                "desired_cmd": post_tool_cmd,
            }
        });

        let mut merged = existing.clone();
        let mut hooks_obj = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));

        for hook_type in &["PreToolUse", "PostToolUse"] {
            let desired_matcher = desired_hooks[hook_type]["matcher"].as_str().unwrap();
            let desired_cmd = desired_hooks[hook_type]["desired_cmd"].as_str().unwrap();

            let mut hook_type_array = hooks_obj
                .get(*hook_type)
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let mut found_matcher_idx: Option<usize> = None;
            for (idx, item) in hook_type_array.iter().enumerate() {
                if let Some(matcher) = item.get("matcher").and_then(|m| m.as_str())
                    && matcher == desired_matcher
                {
                    found_matcher_idx = Some(idx);
                    break;
                }
            }

            let matcher_idx = match found_matcher_idx {
                Some(idx) => idx,
                None => {
                    hook_type_array.push(json!({
                        "matcher": desired_matcher,
                        "hooks": []
                    }));
                    hook_type_array.len() - 1
                }
            };

            let mut hooks_array = hook_type_array[matcher_idx]
                .get("hooks")
                .and_then(|h| h.as_array())
                .cloned()
                .unwrap_or_default();

            let mut found_idx: Option<usize> = None;
            let mut needs_update = false;

            for (idx, hook) in hooks_array.iter().enumerate() {
                if let Some(cmd) = hook.get("command").and_then(|c| c.as_str())
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
                        hooks_array[idx] = json!({
                            "type": "command",
                            "command": desired_cmd
                        });
                    }
                    let keep_idx = idx;
                    let mut current_idx = 0;
                    hooks_array.retain(|hook| {
                        if current_idx == keep_idx {
                            current_idx += 1;
                            true
                        } else if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
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
                    hooks_array.push(json!({
                        "type": "command",
                        "command": desired_cmd
                    }));
                }
            }

            if let Some(matcher_block) = hook_type_array[matcher_idx].as_object_mut() {
                matcher_block.insert("hooks".to_string(), Value::Array(hooks_array));
            }

            if let Some(obj) = hooks_obj.as_object_mut() {
                obj.insert(hook_type.to_string(), Value::Array(hook_type_array));
            }
        }

        if let Some(root) = merged.as_object_mut() {
            root.insert("hooks".to_string(), hooks_obj);
        }

        // Add claudeHooksImported flag if it doesn't exist
        if let Some(hooks) = merged.get_mut("hooks").and_then(|h| h.as_object_mut())
            && !hooks.contains_key("claudeHooksImported")
        {
            hooks.insert("claudeHooksImported".to_string(), json!(true));
        }

        if existing == merged {
            return Ok(None);
        }

        let new_content = serde_json::to_string_pretty(&merged)?;
        let diff_output = generate_diff(&settings_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(&settings_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let settings_path = Self::settings_path();

        if !settings_path.exists() {
            return Ok(None);
        }

        let existing_content = fs::read_to_string(&settings_path)?;
        let existing: Value = serde_json::from_str(&existing_content)?;

        let mut merged = existing.clone();
        let mut hooks_obj = match merged.get("hooks").cloned() {
            Some(h) => h,
            None => return Ok(None),
        };

        let mut changed = false;

        for hook_type in &["PreToolUse", "PostToolUse"] {
            if let Some(hook_type_array) =
                hooks_obj.get_mut(*hook_type).and_then(|v| v.as_array_mut())
            {
                for matcher_block in hook_type_array.iter_mut() {
                    if let Some(hooks_array) = matcher_block
                        .get_mut("hooks")
                        .and_then(|h| h.as_array_mut())
                    {
                        let original_len = hooks_array.len();
                        hooks_array.retain(|hook| {
                            if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                                !is_git_ai_checkpoint_command(cmd)
                            } else {
                                true
                            }
                        });
                        if hooks_array.len() != original_len {
                            changed = true;
                        }
                    }
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
        let diff_output = generate_diff(&settings_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(&settings_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_env() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let settings_path = temp_dir.path().join(".factory").join("settings.json");
        (temp_dir, settings_path)
    }

    #[test]
    fn test_droid_install_hooks_creates_file_from_scratch() {
        let (_temp_dir, settings_path) = setup_test_env();

        if let Some(parent) = settings_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let result = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "^(Edit|Write|Create)$",
                        "hooks": [
                            {
                                "type": "command",
                                "command": format!("git-ai {}", DROID_PRE_TOOL_CMD)
                            }
                        ]
                    }
                ],
                "PostToolUse": [
                    {
                        "matcher": "^(Edit|Write|Create)$",
                        "hooks": [
                            {
                                "type": "command",
                                "command": format!("git-ai {}", DROID_POST_TOOL_CMD)
                            }
                        ]
                    }
                ]
            }
        });

        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&result).unwrap(),
        )
        .unwrap();

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        let hooks = content.get("hooks").unwrap();

        let pre_tool = hooks.get("PreToolUse").unwrap().as_array().unwrap();
        let post_tool = hooks.get("PostToolUse").unwrap().as_array().unwrap();

        assert_eq!(pre_tool.len(), 1);
        assert_eq!(post_tool.len(), 1);

        assert_eq!(
            pre_tool[0].get("matcher").unwrap().as_str().unwrap(),
            "^(Edit|Write|Create)$"
        );
        assert_eq!(
            post_tool[0].get("matcher").unwrap().as_str().unwrap(),
            "^(Edit|Write|Create)$"
        );
    }

    #[test]
    fn test_droid_preserves_other_hooks() {
        let (_temp_dir, settings_path) = setup_test_env();

        if let Some(parent) = settings_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let existing = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "^(Edit|Write|Create)$",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "echo 'before write'"
                            }
                        ]
                    }
                ],
                "PostToolUse": [
                    {
                        "matcher": "^(Edit|Write|Create)$",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "prettier --write"
                            }
                        ]
                    }
                ]
            }
        });

        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let mut content: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        let hooks_obj = content.get_mut("hooks").unwrap();

        let pre_array = hooks_obj
            .get_mut("PreToolUse")
            .unwrap()
            .as_array_mut()
            .unwrap();
        pre_array[0]
            .get_mut("hooks")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!({
                "type": "command",
                "command": format!("git-ai {}", DROID_PRE_TOOL_CMD)
            }));

        let post_array = hooks_obj
            .get_mut("PostToolUse")
            .unwrap()
            .as_array_mut()
            .unwrap();
        post_array[0]
            .get_mut("hooks")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!({
                "type": "command",
                "command": format!("git-ai {}", DROID_POST_TOOL_CMD)
            }));

        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&content).unwrap(),
        )
        .unwrap();

        let result: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        let hooks = result.get("hooks").unwrap();

        let pre_hooks = hooks.get("PreToolUse").unwrap().as_array().unwrap()[0]
            .get("hooks")
            .unwrap()
            .as_array()
            .unwrap();
        let post_hooks = hooks.get("PostToolUse").unwrap().as_array().unwrap()[0]
            .get("hooks")
            .unwrap()
            .as_array()
            .unwrap();

        assert_eq!(pre_hooks.len(), 2);
        assert_eq!(post_hooks.len(), 2);

        assert_eq!(
            pre_hooks[0].get("command").unwrap().as_str().unwrap(),
            "echo 'before write'"
        );
        assert_eq!(
            post_hooks[0].get("command").unwrap().as_str().unwrap(),
            "prettier --write"
        );
    }
}
