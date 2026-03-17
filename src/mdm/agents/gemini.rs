use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{
    binary_exists, generate_diff, home_dir, is_git_ai_checkpoint_command, write_atomic,
};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

// Command patterns for hooks
const GEMINI_BEFORE_TOOL_CMD: &str = "checkpoint gemini --hook-input stdin";
const GEMINI_AFTER_TOOL_CMD: &str = "checkpoint gemini --hook-input stdin";

pub struct GeminiInstaller;

impl GeminiInstaller {
    fn settings_path() -> PathBuf {
        home_dir().join(".gemini").join("settings.json")
    }
}

impl HookInstaller for GeminiInstaller {
    fn name(&self) -> &str {
        "Gemini"
    }

    fn id(&self) -> &str {
        "gemini"
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary = binary_exists("gemini");
        let has_dotfiles = home_dir().join(".gemini").exists();

        if !has_binary && !has_dotfiles {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        // Check if hooks are installed
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
            .and_then(|h| h.get("BeforeTool"))
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

        // Ensure directory exists
        if let Some(dir) = settings_path.parent() {
            fs::create_dir_all(dir)?;
        }

        // Read existing content as string
        let existing_content = if settings_path.exists() {
            fs::read_to_string(&settings_path)?
        } else {
            String::new()
        };

        // Parse existing JSON if present, else start with empty object
        let existing: Value = if existing_content.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&existing_content)?
        };

        // Build commands with absolute path
        let before_tool_cmd = format!(
            "{} {}",
            params.binary_path.display(),
            GEMINI_BEFORE_TOOL_CMD
        );
        let after_tool_cmd = format!("{} {}", params.binary_path.display(), GEMINI_AFTER_TOOL_CMD);

        let desired_hooks = json!({
            "BeforeTool": {
                "matcher": "write_file|replace",
                "desired_cmd": before_tool_cmd,
            },
            "AfterTool": {
                "matcher": "write_file|replace",
                "desired_cmd": after_tool_cmd,
            }
        });

        // Merge desired into existing
        let mut merged = existing.clone();

        // Ensure tools.enableHooks is set to true
        if let Some(tools_obj) = merged.get_mut("tools").and_then(|t| t.as_object_mut()) {
            if tools_obj.get("enableHooks") != Some(&json!(true)) {
                tools_obj.insert("enableHooks".to_string(), json!(true));
            }
        } else if let Some(root) = merged.as_object_mut() {
            root.insert("tools".to_string(), json!({ "enableHooks": true }));
        }

        let mut hooks_obj = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));

        // Process both BeforeTool and AfterTool
        for hook_type in &["BeforeTool", "AfterTool"] {
            let desired_matcher = desired_hooks[hook_type]["matcher"].as_str().unwrap();
            let desired_cmd = desired_hooks[hook_type]["desired_cmd"].as_str().unwrap();

            // Get or create the hooks array for this type
            let mut hook_type_array = hooks_obj
                .get(*hook_type)
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Find existing matcher block for write_file|replace
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

            // Get the hooks array within this matcher block
            let mut hooks_array = hook_type_array[matcher_idx]
                .get("hooks")
                .and_then(|h| h.as_array())
                .cloned()
                .unwrap_or_default();

            // Update outdated git-ai checkpoint commands
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
                    // Remove any duplicate git-ai checkpoint commands
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

            // Write back the hooks array to the matcher block
            if let Some(matcher_block) = hook_type_array[matcher_idx].as_object_mut() {
                matcher_block.insert("hooks".to_string(), Value::Array(hooks_array));
            }

            // Write back the updated hook_type_array
            if let Some(obj) = hooks_obj.as_object_mut() {
                obj.insert(hook_type.to_string(), Value::Array(hook_type_array));
            }
        }

        // Write back hooks to merged
        if let Some(root) = merged.as_object_mut() {
            root.insert("hooks".to_string(), hooks_obj);
        }

        // Check if there are semantic changes (compare JSON values, not strings)
        if existing == merged {
            return Ok(None);
        }

        // Generate new content
        let new_content = serde_json::to_string_pretty(&merged)?;

        // Generate diff
        let diff_output = generate_diff(&settings_path, &existing_content, &new_content);

        // Write if not dry-run
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

        // Remove git-ai checkpoint commands from both BeforeTool and AfterTool
        for hook_type in &["BeforeTool", "AfterTool"] {
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

        // Write back hooks to merged
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
        let settings_path = temp_dir.path().join(".gemini").join("settings.json");
        (temp_dir, settings_path)
    }

    fn create_test_binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    #[test]
    fn test_gemini_install_hooks_creates_file_from_scratch() {
        let (_temp_dir, settings_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        if let Some(parent) = settings_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let result = json!({
            "tools": {
                "enableHooks": true
            },
            "hooks": {
                "BeforeTool": [
                    {
                        "matcher": "write_file|replace",
                        "hooks": [
                            {
                                "type": "command",
                                "command": format!("{} {}", binary_path.display(), GEMINI_BEFORE_TOOL_CMD)
                            }
                        ]
                    }
                ],
                "AfterTool": [
                    {
                        "matcher": "write_file|replace",
                        "hooks": [
                            {
                                "type": "command",
                                "command": format!("{} {}", binary_path.display(), GEMINI_AFTER_TOOL_CMD)
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

        assert_eq!(
            content.get("tools").unwrap().get("enableHooks").unwrap(),
            &json!(true)
        );

        let hooks = content.get("hooks").unwrap();

        let before_tool = hooks.get("BeforeTool").unwrap().as_array().unwrap();
        let after_tool = hooks.get("AfterTool").unwrap().as_array().unwrap();

        assert_eq!(before_tool.len(), 1);
        assert_eq!(after_tool.len(), 1);

        assert_eq!(
            before_tool[0].get("matcher").unwrap().as_str().unwrap(),
            "write_file|replace"
        );
        assert_eq!(
            after_tool[0].get("matcher").unwrap().as_str().unwrap(),
            "write_file|replace"
        );
    }

    #[test]
    fn test_gemini_removes_duplicates() {
        let (_temp_dir, settings_path) = setup_test_env();

        if let Some(parent) = settings_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let existing = json!({
            "tools": {
                "enableHooks": true
            },
            "hooks": {
                "BeforeTool": [
                    {
                        "matcher": "write_file|replace",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "git-ai checkpoint gemini"
                            },
                            {
                                "type": "command",
                                "command": "git-ai checkpoint gemini --hook-input stdin 2>/dev/null || true"
                            }
                        ]
                    }
                ],
                "AfterTool": [
                    {
                        "matcher": "write_file|replace",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "git-ai checkpoint gemini --hook-input \"$(cat)\""
                            },
                            {
                                "type": "command",
                                "command": "git-ai checkpoint gemini --hook-input stdin"
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

        let binary_path = create_test_binary_path();
        let before_tool_cmd = format!("{} {}", binary_path.display(), GEMINI_BEFORE_TOOL_CMD);
        let after_tool_cmd = format!("{} {}", binary_path.display(), GEMINI_AFTER_TOOL_CMD);

        for (hook_type, desired_cmd) in &[
            ("BeforeTool", before_tool_cmd),
            ("AfterTool", after_tool_cmd),
        ] {
            let hooks_obj = content.get_mut("hooks").unwrap();
            let hook_type_array = hooks_obj
                .get_mut(*hook_type)
                .unwrap()
                .as_array_mut()
                .unwrap();
            let matcher_block = &mut hook_type_array[0];
            let hooks_array = matcher_block
                .get_mut("hooks")
                .unwrap()
                .as_array_mut()
                .unwrap();

            let mut found_idx: Option<usize> = None;
            let mut needs_update = false;

            for (idx, hook) in hooks_array.iter().enumerate() {
                if let Some(cmd) = hook.get("command").and_then(|c| c.as_str())
                    && is_git_ai_checkpoint_command(cmd)
                    && found_idx.is_none()
                {
                    found_idx = Some(idx);
                    if cmd != *desired_cmd {
                        needs_update = true;
                    }
                }
            }

            if let Some(idx) = found_idx
                && needs_update
            {
                hooks_array[idx] = json!({
                    "type": "command",
                    "command": desired_cmd
                });
            }

            let first_idx = found_idx;
            if let Some(keep_idx) = first_idx {
                let mut i = 0;
                hooks_array.retain(|hook| {
                    let should_keep = if i == keep_idx {
                        true
                    } else if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                        !is_git_ai_checkpoint_command(cmd)
                    } else {
                        true
                    };
                    i += 1;
                    should_keep
                });
            }
        }

        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&content).unwrap(),
        )
        .unwrap();

        let result: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        let hooks = result.get("hooks").unwrap();

        for hook_type in &["BeforeTool", "AfterTool"] {
            let hook_array = hooks.get(*hook_type).unwrap().as_array().unwrap();
            assert_eq!(hook_array.len(), 1);

            let hooks_in_matcher = hook_array[0].get("hooks").unwrap().as_array().unwrap();
            assert_eq!(
                hooks_in_matcher.len(),
                1,
                "{} should have exactly 1 hook after deduplication",
                hook_type
            );
        }
    }
}
