use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{generate_diff, home_dir, is_git_ai_checkpoint_command, write_atomic};
use std::fs;
use std::path::{Path, PathBuf};

const KIMI_CHECKPOINT_CMD: &str = "checkpoint kimi-code --hook-input stdin";

pub struct KimiCodeInstaller;

impl KimiCodeInstaller {
    fn config_path() -> PathBuf {
        home_dir().join(".kimi").join("config.toml")
    }

    fn desired_command(binary_path: &Path) -> String {
        format!("{} {}", binary_path.display(), KIMI_CHECKPOINT_CMD)
    }

    fn is_git_ai_kimi_command(cmd: &str) -> bool {
        is_git_ai_checkpoint_command(cmd) && cmd.contains("checkpoint kimi-code")
    }

    /// Parse existing [[hooks]] entries from config.toml content.
    /// Returns the full content and a list of (event, command) pairs that are ours.
    fn has_hook_for_event(content: &str, event: &str) -> bool {
        // Parse TOML to check for our hook entries
        let parsed: toml::Value = match toml::from_str(content) {
            Ok(v) => v,
            Err(_) => return false,
        };

        let hooks = match parsed.get("hooks").and_then(|h| h.as_array()) {
            Some(arr) => arr,
            None => return false,
        };

        hooks.iter().any(|hook| {
            let hook_event = hook.get("event").and_then(|v| v.as_str()).unwrap_or("");
            let hook_cmd = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
            hook_event == event && Self::is_git_ai_kimi_command(hook_cmd)
        })
    }
}

impl HookInstaller for KimiCodeInstaller {
    fn name(&self) -> &str {
        "Kimi Code"
    }

    fn id(&self) -> &str {
        "kimi-code"
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let kimi_dir = home_dir().join(".kimi");
        if !kimi_dir.exists() {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let config_path = Self::config_path();
        if !config_path.exists() {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let content = fs::read_to_string(&config_path)?;
        let has_pre = Self::has_hook_for_event(&content, "PreToolUse");
        let has_post = Self::has_hook_for_event(&content, "PostToolUse");

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed: has_pre && has_post,
            hooks_up_to_date: has_pre && has_post,
        })
    }

    fn install_hooks(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let config_path = Self::config_path();
        if let Some(dir) = config_path.parent() {
            fs::create_dir_all(dir)?;
        }

        let existing_content = if config_path.exists() {
            fs::read_to_string(&config_path)?
        } else {
            String::new()
        };

        let desired_cmd = Self::desired_command(&params.binary_path);

        // Parse existing TOML (or start fresh)
        let mut parsed: toml::Value = if existing_content.trim().is_empty() {
            toml::Value::Table(toml::map::Map::new())
        } else {
            toml::from_str(&existing_content).map_err(|e| {
                GitAiError::Generic(format!("Failed to parse Kimi config.toml: {e}"))
            })?
        };

        let hooks = parsed
            .as_table_mut()
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| toml::Value::Array(Vec::new()));

        let hooks_arr = match hooks.as_array_mut() {
            Some(arr) => arr,
            None => {
                return Err(GitAiError::Generic(
                    "Kimi config.toml 'hooks' field is not an array".to_string(),
                ));
            }
        };

        for event in &["PreToolUse", "PostToolUse"] {
            // Check if we already have a hook for this event
            let existing_idx = hooks_arr.iter().position(|hook| {
                let hook_event = hook.get("event").and_then(|v| v.as_str()).unwrap_or("");
                let hook_cmd = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
                hook_event == *event && Self::is_git_ai_kimi_command(hook_cmd)
            });

            match existing_idx {
                Some(idx) => {
                    // Update command if it changed
                    let current_cmd = hooks_arr[idx]
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if current_cmd != desired_cmd {
                        let mut entry = toml::map::Map::new();
                        entry.insert("event".to_string(), toml::Value::String(event.to_string()));
                        entry.insert(
                            "command".to_string(),
                            toml::Value::String(desired_cmd.clone()),
                        );
                        hooks_arr[idx] = toml::Value::Table(entry);
                    }
                }
                None => {
                    // Add new hook entry
                    let mut entry = toml::map::Map::new();
                    entry.insert("event".to_string(), toml::Value::String(event.to_string()));
                    entry.insert(
                        "command".to_string(),
                        toml::Value::String(desired_cmd.clone()),
                    );
                    hooks_arr.push(toml::Value::Table(entry));
                }
            }
        }

        let new_content = toml::to_string_pretty(&parsed).map_err(|e| {
            GitAiError::Generic(format!("Failed to serialize Kimi config.toml: {e}"))
        })?;

        if existing_content == new_content {
            return Ok(None);
        }

        let diff_output = generate_diff(&config_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(&config_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let config_path = Self::config_path();
        if !config_path.exists() {
            return Ok(None);
        }

        let existing_content = fs::read_to_string(&config_path)?;
        let mut parsed: toml::Value = match toml::from_str(&existing_content) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };

        let hooks = match parsed
            .as_table_mut()
            .and_then(|t| t.get_mut("hooks"))
            .and_then(|h| h.as_array_mut())
        {
            Some(arr) => arr,
            None => return Ok(None),
        };

        let original_len = hooks.len();
        hooks.retain(|hook| {
            let cmd = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
            !Self::is_git_ai_kimi_command(cmd)
        });

        if hooks.len() == original_len {
            return Ok(None);
        }

        let new_content = toml::to_string_pretty(&parsed).map_err(|e| {
            GitAiError::Generic(format!("Failed to serialize Kimi config.toml: {e}"))
        })?;

        let diff_output = generate_diff(&config_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(&config_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    fn create_test_binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    fn with_temp_home<F: FnOnce(&Path)>(f: F) {
        let temp_dir = TempDir::new().unwrap();
        let home = temp_dir.path().to_path_buf();

        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");

        // SAFETY: tests using this helper are serialized via #[serial].
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("USERPROFILE", &home);
        }

        f(&home);

        // SAFETY: tests using this helper are serialized via #[serial].
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
    fn test_kimi_command_detection() {
        assert!(KimiCodeInstaller::is_git_ai_kimi_command(
            "/usr/local/bin/git-ai checkpoint kimi-code --hook-input stdin"
        ));
        assert!(!KimiCodeInstaller::is_git_ai_kimi_command(
            "git-ai checkpoint cursor"
        ));
    }

    #[test]
    #[serial]
    fn test_check_hooks_not_installed() {
        with_temp_home(|_home| {
            let installer = KimiCodeInstaller;
            let result = installer
                .check_hooks(&HookInstallerParams {
                    binary_path: create_test_binary_path(),
                })
                .unwrap();
            assert!(!result.tool_installed);
            assert!(!result.hooks_installed);
        });
    }

    #[test]
    #[serial]
    fn test_check_hooks_tool_installed_no_hooks() {
        with_temp_home(|home| {
            fs::create_dir_all(home.join(".kimi")).unwrap();
            fs::write(home.join(".kimi").join("config.toml"), "").unwrap();

            let installer = KimiCodeInstaller;
            let result = installer
                .check_hooks(&HookInstallerParams {
                    binary_path: create_test_binary_path(),
                })
                .unwrap();
            assert!(result.tool_installed);
            assert!(!result.hooks_installed);
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_creates_expected_entries() {
        with_temp_home(|home| {
            fs::create_dir_all(home.join(".kimi")).unwrap();

            let installer = KimiCodeInstaller;
            let diff = installer
                .install_hooks(
                    &HookInstallerParams {
                        binary_path: create_test_binary_path(),
                    },
                    false,
                )
                .unwrap();

            assert!(diff.is_some());

            let content = fs::read_to_string(home.join(".kimi").join("config.toml")).unwrap();
            let parsed: toml::Value = toml::from_str(&content).unwrap();
            let hooks = parsed.get("hooks").unwrap().as_array().unwrap();

            assert_eq!(hooks.len(), 2);
            assert_eq!(
                hooks[0].get("event").unwrap().as_str().unwrap(),
                "PreToolUse"
            );
            assert!(
                hooks[0]
                    .get("command")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .contains("checkpoint kimi-code")
            );
            assert_eq!(
                hooks[1].get("event").unwrap().as_str().unwrap(),
                "PostToolUse"
            );
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_preserves_existing_config() {
        with_temp_home(|home| {
            let kimi_dir = home.join(".kimi");
            fs::create_dir_all(&kimi_dir).unwrap();

            let existing = r#"
model = "moonshot-v1-128k"

[settings]
temperature = 0.7
"#;
            fs::write(kimi_dir.join("config.toml"), existing).unwrap();

            let installer = KimiCodeInstaller;
            installer
                .install_hooks(
                    &HookInstallerParams {
                        binary_path: create_test_binary_path(),
                    },
                    false,
                )
                .unwrap();

            let content = fs::read_to_string(kimi_dir.join("config.toml")).unwrap();
            let parsed: toml::Value = toml::from_str(&content).unwrap();

            // Existing config preserved
            assert_eq!(
                parsed.get("model").unwrap().as_str().unwrap(),
                "moonshot-v1-128k"
            );
            assert!(parsed.get("settings").is_some());

            // Hooks added
            let hooks = parsed.get("hooks").unwrap().as_array().unwrap();
            assert_eq!(hooks.len(), 2);
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_updates_existing_command() {
        with_temp_home(|home| {
            let kimi_dir = home.join(".kimi");
            fs::create_dir_all(&kimi_dir).unwrap();

            let existing = r#"
[[hooks]]
event = "PreToolUse"
command = "/old/path/git-ai checkpoint kimi-code --hook-input stdin"

[[hooks]]
event = "PostToolUse"
command = "/old/path/git-ai checkpoint kimi-code --hook-input stdin"
"#;
            fs::write(kimi_dir.join("config.toml"), existing).unwrap();

            let installer = KimiCodeInstaller;
            let diff = installer
                .install_hooks(
                    &HookInstallerParams {
                        binary_path: create_test_binary_path(),
                    },
                    false,
                )
                .unwrap();

            assert!(diff.is_some());

            let content = fs::read_to_string(kimi_dir.join("config.toml")).unwrap();
            assert!(content.contains("/usr/local/bin/git-ai checkpoint kimi-code"));
            assert!(!content.contains("/old/path/git-ai"));
        });
    }

    #[test]
    #[serial]
    fn test_uninstall_hooks_removes_only_kimi_entries() {
        with_temp_home(|home| {
            let kimi_dir = home.join(".kimi");
            fs::create_dir_all(&kimi_dir).unwrap();

            let existing = r#"
[[hooks]]
event = "PreToolUse"
command = "/usr/local/bin/git-ai checkpoint kimi-code --hook-input stdin"

[[hooks]]
event = "PostToolUse"
command = "echo keep-this"

[[hooks]]
event = "PostToolUse"
command = "/usr/local/bin/git-ai checkpoint kimi-code --hook-input stdin"
"#;
            fs::write(kimi_dir.join("config.toml"), existing).unwrap();

            let installer = KimiCodeInstaller;
            let diff = installer
                .uninstall_hooks(
                    &HookInstallerParams {
                        binary_path: create_test_binary_path(),
                    },
                    false,
                )
                .unwrap();

            assert!(diff.is_some());

            let content = fs::read_to_string(kimi_dir.join("config.toml")).unwrap();
            let parsed: toml::Value = toml::from_str(&content).unwrap();
            let hooks = parsed.get("hooks").unwrap().as_array().unwrap();

            assert_eq!(hooks.len(), 1);
            assert_eq!(
                hooks[0].get("command").unwrap().as_str().unwrap(),
                "echo keep-this"
            );
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_idempotent() {
        with_temp_home(|home| {
            fs::create_dir_all(home.join(".kimi")).unwrap();

            let installer = KimiCodeInstaller;
            let params = HookInstallerParams {
                binary_path: create_test_binary_path(),
            };

            // First install
            let diff1 = installer.install_hooks(&params, false).unwrap();
            assert!(diff1.is_some());

            // Second install should be no-op
            let diff2 = installer.install_hooks(&params, false).unwrap();
            assert!(diff2.is_none());
        });
    }
}
