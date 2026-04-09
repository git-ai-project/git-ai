use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{
    binary_exists, generate_diff, home_dir, is_git_ai_checkpoint_command, write_atomic,
};
use serde_json::{Value as JsonValue, json};
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value as TomlValue;
use toml::map::Map;

const CODEX_CHECKPOINT_CMD: &str = "checkpoint codex --hook-input stdin";
const CODEX_HOOK_EVENTS: [&str; 3] = ["PreToolUse", "PostToolUse", "Stop"];

pub struct CodexInstaller;

impl CodexInstaller {
    fn config_path() -> PathBuf {
        home_dir().join(".codex").join("config.toml")
    }

    fn hooks_path() -> PathBuf {
        home_dir().join(".codex").join("hooks.json")
    }

    fn desired_command(binary_path: &Path) -> String {
        format!("{} {}", binary_path.display(), CODEX_CHECKPOINT_CMD)
    }

    fn parse_config_toml(content: &str) -> Result<TomlValue, GitAiError> {
        if content.trim().is_empty() {
            return Ok(TomlValue::Table(Map::new()));
        }

        let parsed: TomlValue = toml::from_str(content)
            .map_err(|e| GitAiError::Generic(format!("Failed to parse Codex config.toml: {e}")))?;

        if !parsed.is_table() {
            return Err(GitAiError::Generic(
                "Codex config.toml root must be a TOML table".to_string(),
            ));
        }

        Ok(parsed)
    }

    fn parse_hooks_json(content: &str) -> Result<JsonValue, GitAiError> {
        if content.trim().is_empty() {
            return Ok(json!({}));
        }

        let parsed: JsonValue = serde_json::from_str(content)?;
        if !parsed.is_object() {
            return Err(GitAiError::Generic(
                "Codex hooks.json root must be a JSON object".to_string(),
            ));
        }
        Ok(parsed)
    }

    fn notify_args_from_config(config: &TomlValue) -> Option<Vec<String>> {
        let arr = config.get("notify")?.as_array()?;
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            out.push(item.as_str()?.to_string());
        }
        Some(out)
    }

    fn is_git_ai_codex_command(cmd: &str) -> bool {
        is_git_ai_checkpoint_command(cmd) && cmd.contains("checkpoint codex")
    }

    fn is_git_ai_codex_notify_args(args: &[String]) -> bool {
        if args.len() < 4 {
            return false;
        }

        let has_git_ai_bin = args
            .first()
            .map(|bin| {
                bin == "git-ai"
                    || bin.ends_with("/git-ai")
                    || bin.ends_with("\\git-ai")
                    || bin.ends_with("/git-ai.exe")
                    || bin.ends_with("\\git-ai.exe")
            })
            .unwrap_or(false);

        let has_checkpoint_codex = args
            .windows(2)
            .any(|window| window[0] == "checkpoint" && window[1] == "codex");
        let has_hook_input = args.iter().any(|arg| arg == "--hook-input");

        has_git_ai_bin && has_checkpoint_codex && has_hook_input
    }

    fn config_hooks_enabled(config: &TomlValue) -> bool {
        config
            .get("features")
            .and_then(|v| v.get("codex_hooks"))
            .and_then(|v| v.as_bool())
            == Some(true)
    }

    fn config_with_installed_hooks(
        config: &TomlValue,
        _binary_path: &Path,
    ) -> Result<TomlValue, GitAiError> {
        let mut merged = Self::remove_notify_if_git_ai(config)?.unwrap_or(config.clone());
        let root = merged
            .as_table_mut()
            .ok_or_else(|| GitAiError::Generic("Codex config root must be a table".to_string()))?;

        if let Some(features) = root.get_mut("features").and_then(|v| v.as_table_mut()) {
            features.insert("codex_hooks".to_string(), TomlValue::Boolean(true));
        } else {
            root.insert(
                "features".to_string(),
                TomlValue::Table(Map::from_iter([(
                    "codex_hooks".to_string(),
                    TomlValue::Boolean(true),
                )])),
            );
        }

        Ok(merged)
    }

    fn remove_notify_if_git_ai(config: &TomlValue) -> Result<Option<TomlValue>, GitAiError> {
        let Some(notify_args) = Self::notify_args_from_config(config) else {
            return Ok(None);
        };

        if !Self::is_git_ai_codex_notify_args(&notify_args) {
            return Ok(None);
        }

        let mut merged = config.clone();
        let root = merged
            .as_table_mut()
            .ok_or_else(|| GitAiError::Generic("Codex config root must be a table".to_string()))?;
        root.remove("notify");
        Ok(Some(merged))
    }

    fn hooks_have_codex_commands(hooks_json: &JsonValue, require_catch_all: bool) -> bool {
        CODEX_HOOK_EVENTS.iter().all(|event_name| {
            hooks_json
                .get("hooks")
                .and_then(|hooks| hooks.get(*event_name))
                .and_then(|value| value.as_array())
                .map(|blocks| {
                    blocks.iter().any(|block| {
                        let is_catch_all = block.get("matcher").is_none()
                            || block.get("matcher").and_then(|v| v.as_str()) == Some("*");
                        (!require_catch_all || is_catch_all)
                            && block
                                .get("hooks")
                                .and_then(|value| value.as_array())
                                .map(|hooks| {
                                    hooks.iter().any(|hook| {
                                        hook.get("command")
                                            .and_then(|value| value.as_str())
                                            .map(Self::is_git_ai_codex_command)
                                            .unwrap_or(false)
                                    })
                                })
                                .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        })
    }

    fn hooks_with_installed_commands(
        hooks_json: &JsonValue,
        binary_path: &Path,
    ) -> Result<JsonValue, GitAiError> {
        let mut merged = hooks_json.clone();
        let root = merged.as_object_mut().ok_or_else(|| {
            GitAiError::Generic("Codex hooks.json root must be a JSON object".to_string())
        })?;
        let hooks_entry = root.entry("hooks".to_string()).or_insert_with(|| json!({}));
        if !hooks_entry.is_object() {
            *hooks_entry = json!({});
        }
        let hooks_obj = hooks_entry.as_object_mut().ok_or_else(|| {
            GitAiError::Generic("Codex hooks field must be a JSON object".to_string())
        })?;
        let desired_command = Self::desired_command(binary_path);

        for event_name in CODEX_HOOK_EVENTS {
            let blocks = hooks_obj
                .get(event_name)
                .and_then(|value| value.as_array())
                .cloned()
                .unwrap_or_default();
            let mut cleaned_blocks = Vec::new();

            for block in blocks {
                let mut cleaned_block = block;
                let original_hook_count = cleaned_block
                    .get("hooks")
                    .and_then(|value| value.as_array())
                    .map(|hooks| hooks.len())
                    .unwrap_or(0);

                let cleaned_hooks = cleaned_block
                    .get("hooks")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|hook| {
                        hook.get("command")
                            .and_then(|value| value.as_str())
                            .map(|cmd| !Self::is_git_ai_codex_command(cmd))
                            .unwrap_or(true)
                    })
                    .collect::<Vec<_>>();

                if let Some(block_obj) = cleaned_block.as_object_mut() {
                    block_obj.insert("hooks".to_string(), JsonValue::Array(cleaned_hooks));
                }

                let remaining_hook_count = cleaned_block
                    .get("hooks")
                    .and_then(|value| value.as_array())
                    .map(|hooks| hooks.len())
                    .unwrap_or(0);
                if remaining_hook_count > 0 || original_hook_count == 0 {
                    cleaned_blocks.push(cleaned_block);
                }
            }

            let target_idx = cleaned_blocks
                .iter()
                .position(|block| block.get("matcher").is_none())
                .unwrap_or_else(|| {
                    cleaned_blocks.push(json!({ "hooks": [] }));
                    cleaned_blocks.len() - 1
                });

            if let Some(hooks_array) = cleaned_blocks[target_idx]
                .get_mut("hooks")
                .and_then(|value| value.as_array_mut())
            {
                hooks_array.push(json!({
                    "type": "command",
                    "command": desired_command
                }));
            }

            hooks_obj.insert(event_name.to_string(), JsonValue::Array(cleaned_blocks));
        }

        Ok(merged)
    }

    fn remove_codex_hooks_from_json(
        hooks_json: &JsonValue,
    ) -> Result<(JsonValue, bool), GitAiError> {
        let mut merged = hooks_json.clone();
        let root = merged.as_object_mut().ok_or_else(|| {
            GitAiError::Generic("Codex hooks.json root must be a JSON object".to_string())
        })?;
        let Some(hooks_entry) = root.get_mut("hooks") else {
            return Ok((merged, false));
        };
        if !hooks_entry.is_object() {
            return Ok((merged, false));
        }
        let hooks_obj = hooks_entry.as_object_mut().ok_or_else(|| {
            GitAiError::Generic("Codex hooks field must be a JSON object".to_string())
        })?;

        let mut changed = false;
        for event_name in CODEX_HOOK_EVENTS {
            let Some(blocks) = hooks_obj.get(event_name).and_then(|value| value.as_array()) else {
                continue;
            };

            let mut cleaned_blocks = Vec::new();
            for block in blocks.clone() {
                let mut cleaned_block = block;
                let original_hook_count = cleaned_block
                    .get("hooks")
                    .and_then(|value| value.as_array())
                    .map(|hooks| hooks.len())
                    .unwrap_or(0);
                let cleaned_hooks = cleaned_block
                    .get("hooks")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|hook| {
                        hook.get("command")
                            .and_then(|value| value.as_str())
                            .map(|cmd| !Self::is_git_ai_codex_command(cmd))
                            .unwrap_or(true)
                    })
                    .collect::<Vec<_>>();
                if cleaned_hooks.len() != original_hook_count {
                    changed = true;
                }

                if let Some(block_obj) = cleaned_block.as_object_mut() {
                    block_obj.insert("hooks".to_string(), JsonValue::Array(cleaned_hooks));
                }

                let remaining_hook_count = cleaned_block
                    .get("hooks")
                    .and_then(|value| value.as_array())
                    .map(|hooks| hooks.len())
                    .unwrap_or(0);
                if remaining_hook_count > 0 {
                    cleaned_blocks.push(cleaned_block);
                }
            }

            if cleaned_blocks.is_empty() {
                hooks_obj.remove(event_name);
            } else {
                hooks_obj.insert(event_name.to_string(), JsonValue::Array(cleaned_blocks));
            }
        }

        Ok((merged, changed))
    }

    fn hooks_json_has_any_entries(hooks_json: &JsonValue) -> bool {
        hooks_json
            .get("hooks")
            .and_then(|value| value.as_object())
            .map(|hooks| {
                hooks.values().any(|value| {
                    value
                        .as_array()
                        .map(|blocks| !blocks.is_empty())
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    }

    fn remove_feature_flag_if_unused(
        config: &TomlValue,
        keep_codex_hooks_enabled: bool,
    ) -> Result<TomlValue, GitAiError> {
        let mut merged = config.clone();
        let root = merged
            .as_table_mut()
            .ok_or_else(|| GitAiError::Generic("Codex config root must be a table".to_string()))?;

        if keep_codex_hooks_enabled {
            return Ok(merged);
        }

        if let Some(features) = root
            .get_mut("features")
            .and_then(|value| value.as_table_mut())
        {
            features.remove("codex_hooks");
            if features.is_empty() {
                root.remove("features");
            }
        }

        Ok(merged)
    }
}

impl HookInstaller for CodexInstaller {
    fn name(&self) -> &str {
        "Codex"
    }

    fn id(&self) -> &str {
        "codex"
    }

    fn process_names(&self) -> Vec<&str> {
        vec!["codex"]
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary = binary_exists("codex");
        let has_dotfiles = home_dir().join(".codex").exists();

        if !has_binary && !has_dotfiles {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let config_path = Self::config_path();
        let hooks_path = Self::hooks_path();
        let config = if config_path.exists() {
            Self::parse_config_toml(&fs::read_to_string(&config_path)?)?
        } else {
            TomlValue::Table(Map::new())
        };
        let hooks_json = if hooks_path.exists() {
            Self::parse_hooks_json(&fs::read_to_string(&hooks_path)?)?
        } else {
            json!({})
        };

        let desired_config = Self::config_with_installed_hooks(&config, &params.binary_path)?;
        let desired_hooks = Self::hooks_with_installed_commands(&hooks_json, &params.binary_path)?;
        let hooks_installed = Self::config_hooks_enabled(&config)
            && Self::hooks_have_codex_commands(&hooks_json, false);
        let hooks_up_to_date = config == desired_config && hooks_json == desired_hooks;

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed,
            hooks_up_to_date,
        })
    }

    fn install_hooks(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let config_path = Self::config_path();
        let hooks_path = Self::hooks_path();

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = hooks_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let existing_config_content = if config_path.exists() {
            fs::read_to_string(&config_path)?
        } else {
            String::new()
        };
        let existing_hooks_content = if hooks_path.exists() {
            fs::read_to_string(&hooks_path)?
        } else {
            String::new()
        };

        let existing_config = Self::parse_config_toml(&existing_config_content)?;
        let existing_hooks = Self::parse_hooks_json(&existing_hooks_content)?;
        let merged_config =
            Self::config_with_installed_hooks(&existing_config, &params.binary_path)?;
        let merged_hooks =
            Self::hooks_with_installed_commands(&existing_hooks, &params.binary_path)?;

        if existing_config == merged_config && existing_hooks == merged_hooks {
            return Ok(None);
        }

        let new_config_content = toml::to_string_pretty(&merged_config).map_err(|e| {
            GitAiError::Generic(format!("Failed to serialize Codex config.toml: {e}"))
        })?;
        let new_hooks_content = serde_json::to_string_pretty(&merged_hooks)?;
        let mut diff_output = Vec::new();
        if existing_config != merged_config {
            diff_output.push(generate_diff(
                &config_path,
                &existing_config_content,
                &new_config_content,
            ));
        }
        if existing_hooks != merged_hooks {
            diff_output.push(generate_diff(
                &hooks_path,
                &existing_hooks_content,
                &new_hooks_content,
            ));
        }

        if !dry_run {
            if existing_config != merged_config {
                write_atomic(&config_path, new_config_content.as_bytes())?;
            }
            if existing_hooks != merged_hooks {
                write_atomic(&hooks_path, new_hooks_content.as_bytes())?;
            }
        }

        Ok(Some(diff_output.join("\n")))
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let config_path = Self::config_path();
        let hooks_path = Self::hooks_path();
        if !config_path.exists() && !hooks_path.exists() {
            return Ok(None);
        }

        let existing_config_content = if config_path.exists() {
            fs::read_to_string(&config_path)?
        } else {
            String::new()
        };
        let existing_hooks_content = if hooks_path.exists() {
            fs::read_to_string(&hooks_path)?
        } else {
            String::new()
        };
        let existing_config = Self::parse_config_toml(&existing_config_content)?;
        let existing_hooks = Self::parse_hooks_json(&existing_hooks_content)?;

        let config_without_notify =
            Self::remove_notify_if_git_ai(&existing_config)?.unwrap_or(existing_config.clone());
        let (merged_hooks, hooks_changed) = Self::remove_codex_hooks_from_json(&existing_hooks)?;
        let merged_config = Self::remove_feature_flag_if_unused(
            &config_without_notify,
            Self::hooks_json_has_any_entries(&merged_hooks),
        )?;

        let config_changed = merged_config != existing_config;
        if !config_changed && !hooks_changed {
            return Ok(None);
        }

        let new_config_content = toml::to_string_pretty(&merged_config).map_err(|e| {
            GitAiError::Generic(format!("Failed to serialize Codex config.toml: {e}"))
        })?;
        let new_hooks_content = serde_json::to_string_pretty(&merged_hooks)?;
        let mut diff_output = Vec::new();
        if config_changed {
            diff_output.push(generate_diff(
                &config_path,
                &existing_config_content,
                &new_config_content,
            ));
        }
        if hooks_changed {
            diff_output.push(generate_diff(
                &hooks_path,
                &existing_hooks_content,
                &new_hooks_content,
            ));
        }

        if !dry_run {
            if config_changed {
                write_atomic(&config_path, new_config_content.as_bytes())?;
            }
            if hooks_changed {
                write_atomic(&hooks_path, new_hooks_content.as_bytes())?;
            }
        }

        Ok(Some(diff_output.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdm::hook_installer::{HookInstaller, HookInstallerParams};
    use serial_test::serial;
    use std::path::Path;
    use tempfile::tempdir;

    fn test_binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    fn with_temp_home<F: FnOnce(&Path)>(f: F) {
        let temp = tempdir().unwrap();
        let home = temp.path().to_path_buf();

        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");

        // SAFETY: tests are serialized via #[serial], so mutating process env is safe.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("USERPROFILE", &home);
        }

        f(&home);

        // SAFETY: tests are serialized via #[serial], so restoring process env is safe.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match prev_userprofile {
                Some(v) => std::env::set_var("USERPROFILE", v),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }

    #[test]
    fn test_is_git_ai_codex_notify_args_true_for_absolute_binary() {
        let args = vec![
            "/usr/local/bin/git-ai".to_string(),
            "checkpoint".to_string(),
            "codex".to_string(),
            "--hook-input".to_string(),
        ];

        assert!(CodexInstaller::is_git_ai_codex_notify_args(&args));
    }

    #[test]
    fn test_is_git_ai_codex_notify_args_true_for_legacy_via_codex_notify_args() {
        let args = vec![
            "/Users/svarlamov/.git-ai/bin/git-ai".to_string(),
            "checkpoint".to_string(),
            "codex".to_string(),
            "--via-codex-notify".to_string(),
            "--hook-input".to_string(),
            "stdin".to_string(),
        ];

        assert!(CodexInstaller::is_git_ai_codex_notify_args(&args));
    }

    #[test]
    fn test_is_git_ai_codex_notify_args_false_for_non_git_ai_command() {
        let args = vec![
            "notify-send".to_string(),
            "Codex".to_string(),
            "done".to_string(),
        ];

        assert!(!CodexInstaller::is_git_ai_codex_notify_args(&args));
    }

    #[test]
    fn test_remove_notify_if_git_ai_removes_only_git_ai_notify() {
        let config = CodexInstaller::parse_config_toml(
            r#"
model = "gpt-5"
notify = ["/usr/local/bin/git-ai", "checkpoint", "codex", "--hook-input"]
"#,
        )
        .unwrap();

        let merged = CodexInstaller::remove_notify_if_git_ai(&config)
            .unwrap()
            .expect("notify should be removed");
        assert!(merged.get("notify").is_none());
        assert_eq!(merged.get("model").and_then(|v| v.as_str()), Some("gpt-5"));
    }

    #[test]
    fn test_remove_notify_if_git_ai_removes_legacy_via_codex_notify_args() {
        let config = CodexInstaller::parse_config_toml(
            r#"
model = "gpt-5"
notify = ["/Users/svarlamov/.git-ai/bin/git-ai", "checkpoint", "codex", "--via-codex-notify", "--hook-input", "stdin"]
"#,
        )
        .unwrap();

        let merged = CodexInstaller::remove_notify_if_git_ai(&config)
            .unwrap()
            .expect("legacy git-ai notify should be removed");
        assert!(merged.get("notify").is_none());
        assert_eq!(merged.get("model").and_then(|v| v.as_str()), Some("gpt-5"));
    }

    #[test]
    fn test_remove_notify_if_git_ai_preserves_custom_notify() {
        let config = CodexInstaller::parse_config_toml(
            r#"
model = "gpt-5"
notify = ["notify-send", "Codex"]
"#,
        )
        .unwrap();

        let merged = CodexInstaller::remove_notify_if_git_ai(&config).unwrap();
        assert!(
            merged.is_none(),
            "Custom notify config should remain untouched"
        );
    }

    #[test]
    fn test_config_with_installed_hooks_enables_feature_flag_and_removes_git_ai_notify() {
        let existing = CodexInstaller::parse_config_toml(
            r#"
model = "gpt-5"
notify = ["/usr/local/bin/git-ai", "checkpoint", "codex", "--hook-input"]
"#,
        )
        .unwrap();

        let merged =
            CodexInstaller::config_with_installed_hooks(&existing, &test_binary_path()).unwrap();
        assert!(CodexInstaller::notify_args_from_config(&merged).is_none());
        assert_eq!(
            merged
                .get("features")
                .and_then(|value| value.get("codex_hooks"))
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            merged.get("model").and_then(|value| value.as_str()),
            Some("gpt-5"),
            "other config should be preserved"
        );
    }

    #[test]
    fn test_hooks_with_installed_commands_adds_unscoped_pre_post_stop_hooks() {
        let existing = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            { "type": "command", "command": "/old/git-ai checkpoint codex --hook-input stdin" },
                            { "type": "command", "command": "echo keep-me" }
                        ]
                    }
                ]
            }
        });

        let merged =
            CodexInstaller::hooks_with_installed_commands(&existing, &test_binary_path()).unwrap();

        for event_name in CODEX_HOOK_EVENTS {
            let blocks = merged["hooks"][event_name]
                .as_array()
                .expect("event blocks should exist");
            assert!(
                blocks.iter().any(|block| {
                    block.get("matcher").is_none()
                        && block["hooks"].as_array().is_some_and(|hooks| {
                            hooks.iter().any(|hook| {
                                hook["command"]
                                    .as_str()
                                    .map(|cmd| {
                                        cmd == CodexInstaller::desired_command(&test_binary_path())
                                    })
                                    .unwrap_or(false)
                            })
                        })
                }),
                "expected unscoped git-ai block for {event_name}"
            );
        }

        let pre_blocks = merged["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(
            pre_blocks.iter().any(|block| {
                block.get("matcher").and_then(|value| value.as_str()) == Some("Bash")
                    && block["hooks"].as_array().is_some_and(|hooks| {
                        hooks
                            .iter()
                            .any(|hook| hook["command"].as_str() == Some("echo keep-me"))
                    })
            }),
            "non-git-ai matched hooks should be preserved"
        );
    }

    #[test]
    fn test_remove_codex_hooks_from_json_removes_only_git_ai_entries() {
        let existing = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "hooks": [
                            { "type": "command", "command": "/usr/local/bin/git-ai checkpoint codex --hook-input stdin" },
                            { "type": "command", "command": "echo keep" }
                        ]
                    }
                ],
                "Stop": [
                    {
                        "hooks": [
                            { "type": "command", "command": "/usr/local/bin/git-ai checkpoint codex --hook-input stdin" }
                        ]
                    }
                ]
            }
        });

        let (merged, changed) = CodexInstaller::remove_codex_hooks_from_json(&existing).unwrap();
        assert!(changed);
        assert_eq!(
            merged["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str(),
            Some("echo keep")
        );
        assert!(
            merged["hooks"].get("Stop").is_none(),
            "empty event arrays should be removed"
        );
    }

    #[test]
    #[serial]
    fn test_install_hooks_updates_config_and_check_reports_up_to_date() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
            let hooks_path = codex_dir.join("hooks.json");
            fs::write(&config_path, "model = \"gpt-5\"\n").unwrap();

            let installer = CodexInstaller;
            let params = HookInstallerParams {
                binary_path: test_binary_path(),
            };

            let diff = installer
                .install_hooks(&params, false)
                .expect("install should succeed");
            assert!(diff.is_some(), "install should report a config diff");

            let content = fs::read_to_string(&config_path).unwrap();
            let parsed = CodexInstaller::parse_config_toml(&content).unwrap();
            assert!(CodexInstaller::notify_args_from_config(&parsed).is_none());
            assert_eq!(
                parsed
                    .get("features")
                    .and_then(|value| value.get("codex_hooks"))
                    .and_then(|value| value.as_bool()),
                Some(true)
            );

            let hooks_json: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
            assert!(CodexInstaller::hooks_have_codex_commands(&hooks_json, true));

            let check = installer
                .check_hooks(&params)
                .expect("check should succeed");
            assert!(check.tool_installed);
            assert!(check.hooks_installed);
            assert!(check.hooks_up_to_date);
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_migrates_git_ai_notify_to_hooks_json_and_enables_feature_flag() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
            let hooks_path = codex_dir.join("hooks.json");
            fs::write(
                &config_path,
                r#"
model = "gpt-5"
notify = ["/usr/local/bin/git-ai", "checkpoint", "codex", "--hook-input"]
"#,
            )
            .unwrap();

            let installer = CodexInstaller;
            let params = HookInstallerParams {
                binary_path: test_binary_path(),
            };

            installer
                .install_hooks(&params, false)
                .expect("install should succeed");

            let config_content = fs::read_to_string(&config_path).unwrap();
            let parsed = CodexInstaller::parse_config_toml(&config_content).unwrap();
            assert!(
                CodexInstaller::notify_args_from_config(&parsed).is_none(),
                "git-ai notify should be removed during migration"
            );
            assert_eq!(
                parsed
                    .get("features")
                    .and_then(|v| v.get("codex_hooks"))
                    .and_then(|v| v.as_bool()),
                Some(true),
                "install should enable codex_hooks feature flag"
            );

            let hooks_content = fs::read_to_string(&hooks_path).unwrap();
            let hooks_json: serde_json::Value = serde_json::from_str(&hooks_content).unwrap();
            for event_name in ["PreToolUse", "PostToolUse", "Stop"] {
                let event_hooks = hooks_json["hooks"][event_name]
                    .as_array()
                    .expect("event hook array should exist");
                assert_eq!(event_hooks.len(), 1);
                let matcher_block = &event_hooks[0];
                assert!(
                    matcher_block.get("matcher").is_none(),
                    "git-ai Codex hooks should not set a matcher"
                );
                let command = matcher_block["hooks"][0]["command"]
                    .as_str()
                    .expect("command should exist");
                assert!(
                    command.contains("checkpoint codex --hook-input stdin"),
                    "unexpected command for {event_name}: {command}"
                );
            }
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_migrates_legacy_via_codex_notify_to_hooks_json() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
            fs::write(
                &config_path,
                r#"
model = "gpt-5"
notify = ["/Users/svarlamov/.git-ai/bin/git-ai", "checkpoint", "codex", "--via-codex-notify", "--hook-input", "stdin"]
"#,
            )
            .unwrap();

            let installer = CodexInstaller;
            let params = HookInstallerParams {
                binary_path: test_binary_path(),
            };

            installer
                .install_hooks(&params, false)
                .expect("install should succeed");

            let config_content = fs::read_to_string(&config_path).unwrap();
            let parsed = CodexInstaller::parse_config_toml(&config_content).unwrap();
            assert!(
                CodexInstaller::notify_args_from_config(&parsed).is_none(),
                "legacy git-ai notify should be removed during migration"
            );
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_preserves_custom_notify_while_adding_hooks_json() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
            let hooks_path = codex_dir.join("hooks.json");
            fs::write(
                &config_path,
                r#"
model = "gpt-5"
notify = ["notify-send", "Codex finished"]
"#,
            )
            .unwrap();

            let installer = CodexInstaller;
            let params = HookInstallerParams {
                binary_path: test_binary_path(),
            };

            installer
                .install_hooks(&params, false)
                .expect("install should succeed");

            let config_content = fs::read_to_string(&config_path).unwrap();
            let parsed = CodexInstaller::parse_config_toml(&config_content).unwrap();
            assert_eq!(
                CodexInstaller::notify_args_from_config(&parsed),
                Some(vec![
                    "notify-send".to_string(),
                    "Codex finished".to_string(),
                ]),
                "non-git-ai notify must be preserved"
            );
            assert_eq!(
                parsed
                    .get("features")
                    .and_then(|v| v.get("codex_hooks"))
                    .and_then(|v| v.as_bool()),
                Some(true),
                "install should still enable codex_hooks"
            );

            let hooks_content = fs::read_to_string(&hooks_path).unwrap();
            let hooks_json: serde_json::Value = serde_json::from_str(&hooks_content).unwrap();
            assert!(hooks_json["hooks"]["PreToolUse"].is_array());
            assert!(hooks_json["hooks"]["PostToolUse"].is_array());
            assert!(hooks_json["hooks"]["Stop"].is_array());
        });
    }

    #[test]
    fn test_parse_config_toml_malformed() {
        let result = CodexInstaller::parse_config_toml("invalid [[ toml");
        assert!(result.is_err(), "Malformed TOML should return Err");
    }

    #[test]
    fn test_parse_config_toml_non_table_root() {
        // A bare integer is a valid TOML value but not a table at root level,
        // so from_str will fail (TOML requires key-value pairs at the top level).
        let result = CodexInstaller::parse_config_toml("42");
        assert!(result.is_err(), "Non-table root value should return Err");
    }

    #[test]
    #[serial]
    fn test_install_hooks_dry_run() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
            let hooks_path = codex_dir.join("hooks.json");
            let original_content = "model = \"gpt-5\"\n";
            fs::write(&config_path, original_content).unwrap();

            let installer = CodexInstaller;
            let params = HookInstallerParams {
                binary_path: test_binary_path(),
            };

            let diff = installer
                .install_hooks(&params, true)
                .expect("dry-run install should succeed");
            assert!(diff.is_some(), "dry-run should still produce a diff");

            // The file must NOT have been modified.
            let after = fs::read_to_string(&config_path).unwrap();
            assert_eq!(
                after, original_content,
                "File should remain unchanged after dry-run install"
            );
            assert!(
                !hooks_path.exists(),
                "hooks.json should not be created during dry-run"
            );
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_idempotent() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
            let hooks_path = codex_dir.join("hooks.json");
            fs::write(&config_path, "model = \"gpt-5\"\n").unwrap();

            let installer = CodexInstaller;
            let params = HookInstallerParams {
                binary_path: test_binary_path(),
            };

            // First install (real write).
            let first = installer
                .install_hooks(&params, false)
                .expect("first install should succeed");
            assert!(first.is_some(), "first install should report changes");

            // Second install should be a no-op.
            let second = installer
                .install_hooks(&params, false)
                .expect("second install should succeed");
            assert!(
                second.is_none(),
                "second install should return None (no changes needed)"
            );

            let hooks_json: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
            assert!(CodexInstaller::hooks_have_codex_commands(&hooks_json, true));
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_migrates_matched_git_ai_hooks_to_unscoped_hooks() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
            let hooks_path = codex_dir.join("hooks.json");
            fs::write(
                &config_path,
                r#"
model = "gpt-5"
"#,
            )
            .unwrap();
            fs::write(
                &hooks_path,
                serde_json::to_string_pretty(&json!({
                    "hooks": {
                        "PreToolUse": [
                            {
                                "matcher": "Bash",
                                "hooks": [
                                    { "type": "command", "command": "/old/git-ai checkpoint codex --hook-input stdin" },
                                    { "type": "command", "command": "echo keep" }
                                ]
                            }
                        ]
                    }
                }))
                .unwrap(),
            )
            .unwrap();

            let installer = CodexInstaller;
            let params = HookInstallerParams {
                binary_path: test_binary_path(),
            };

            installer
                .install_hooks(&params, false)
                .expect("install should succeed");

            let hooks_json: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
            let pre_blocks = hooks_json["hooks"]["PreToolUse"].as_array().unwrap();
            assert!(
                pre_blocks.iter().any(|block| {
                    block.get("matcher").is_none()
                        && block["hooks"].as_array().is_some_and(|hooks| {
                            hooks.iter().any(|hook| {
                                hook["command"]
                                    .as_str()
                                    .map(CodexInstaller::is_git_ai_codex_command)
                                    .unwrap_or(false)
                            })
                        })
                }),
                "git-ai hook should be rewritten into an unscoped block"
            );
            assert!(
                pre_blocks.iter().any(|block| {
                    block.get("matcher").and_then(|value| value.as_str()) == Some("Bash")
                        && block["hooks"].as_array().is_some_and(|hooks| {
                            hooks
                                .iter()
                                .any(|hook| hook["command"].as_str() == Some("echo keep"))
                        })
                }),
                "custom matched hooks should be preserved"
            );
        });
    }

    #[test]
    #[serial]
    fn test_uninstall_hooks_removes_git_ai_entries_and_feature_flag() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
            let hooks_path = codex_dir.join("hooks.json");
            fs::write(
                &config_path,
                r#"
model = "gpt-5"
[features]
codex_hooks = true
"#,
            )
            .unwrap();
            fs::write(
                &hooks_path,
                serde_json::to_string_pretty(&json!({
                    "hooks": {
                        "PreToolUse": [{ "hooks": [{ "type": "command", "command": "/usr/local/bin/git-ai checkpoint codex --hook-input stdin" }] }],
                        "PostToolUse": [{ "hooks": [{ "type": "command", "command": "/usr/local/bin/git-ai checkpoint codex --hook-input stdin" }] }],
                        "Stop": [{ "hooks": [{ "type": "command", "command": "/usr/local/bin/git-ai checkpoint codex --hook-input stdin" }] }],
                    }
                }))
                .unwrap(),
            )
            .unwrap();

            let installer = CodexInstaller;
            let params = HookInstallerParams {
                binary_path: test_binary_path(),
            };

            let diff = installer
                .uninstall_hooks(&params, false)
                .expect("uninstall should succeed");
            assert!(diff.is_some(), "uninstall should report a diff");

            let parsed =
                CodexInstaller::parse_config_toml(&fs::read_to_string(&config_path).unwrap())
                    .unwrap();
            assert!(
                parsed.get("features").is_none(),
                "codex_hooks feature flag should be removed when no hooks remain"
            );
            let hooks_json: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
            assert!(
                !CodexInstaller::hooks_have_codex_commands(&hooks_json, false),
                "git-ai Codex hooks should be removed"
            );
        });
    }

    /// Regression test for #1039: install_hooks should succeed even when
    /// ~/.codex/ directory does not yet exist.
    #[test]
    #[serial]
    fn test_install_hooks_creates_missing_codex_dir() {
        with_temp_home(|home| {
            // Ensure ~/.codex/ does NOT exist
            let codex_dir = home.join(".codex");
            assert!(!codex_dir.exists());

            let installer = CodexInstaller;
            let params = HookInstallerParams {
                binary_path: test_binary_path(),
            };

            let result = installer.install_hooks(&params, false).unwrap();
            assert!(result.is_some(), "should report changes for fresh install");

            // Both config.toml and hooks.json should be created
            let config_path = codex_dir.join("config.toml");
            let hooks_path = codex_dir.join("hooks.json");
            assert!(config_path.exists(), "config.toml should be created");
            assert!(hooks_path.exists(), "hooks.json should be created");

            // Verify hooks.json has the expected structure
            let hooks_json: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
            assert!(
                CodexInstaller::hooks_have_codex_commands(&hooks_json, false),
                "hooks.json should contain git-ai codex commands"
            );
        });
    }
}
