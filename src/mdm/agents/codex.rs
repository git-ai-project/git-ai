use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{binary_exists, generate_diff, home_dir, write_atomic};
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;
use toml::map::Map;

pub struct CodexInstaller;

impl CodexInstaller {
    fn config_path() -> PathBuf {
        home_dir().join(".codex").join("config.toml")
    }

    fn desired_notify_args(binary_path: &Path) -> Vec<String> {
        vec![
            binary_path.display().to_string(),
            "checkpoint".to_string(),
            "codex".to_string(),
            "--hook-input".to_string(),
        ]
    }

    fn parse_config_toml(content: &str) -> Result<Value, GitAiError> {
        if content.trim().is_empty() {
            return Ok(Value::Table(Map::new()));
        }

        let parsed: Value = toml::from_str(content)
            .map_err(|e| GitAiError::Generic(format!("Failed to parse Codex config.toml: {e}")))?;

        if !parsed.is_table() {
            return Err(GitAiError::Generic(
                "Codex config.toml root must be a TOML table".to_string(),
            ));
        }

        Ok(parsed)
    }

    fn notify_args_from_config(config: &Value) -> Option<Vec<String>> {
        let arr = config.get("notify")?.as_array()?;
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            out.push(item.as_str()?.to_string());
        }
        Some(out)
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

        has_git_ai_bin
            && args.windows(3).any(|window| {
                window[0] == "checkpoint" && window[1] == "codex" && window[2] == "--hook-input"
            })
    }

    fn apply_notify(config: &Value, notify_args: &[String]) -> Result<Value, GitAiError> {
        let mut merged = config.clone();
        let root = merged
            .as_table_mut()
            .ok_or_else(|| GitAiError::Generic("Codex config root must be a table".to_string()))?;

        let merged_notify = match Self::notify_args_from_config(config) {
            Some(existing_notify) if Self::is_git_ai_codex_notify_args(&existing_notify) => {
                let mut merged_notify = existing_notify;
                if let Some(binary_path) = merged_notify.first_mut()
                    && *binary_path != notify_args[0]
                {
                    *binary_path = notify_args[0].clone();
                }
                merged_notify
            }
            _ => notify_args.to_vec(),
        };

        root.insert(
            "notify".to_string(),
            Value::Array(merged_notify.into_iter().map(Value::String).collect()),
        );

        Ok(merged)
    }

    fn remove_notify_if_git_ai(config: &Value) -> Result<Option<Value>, GitAiError> {
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
}

impl HookInstaller for CodexInstaller {
    fn name(&self) -> &str {
        "Codex"
    }

    fn id(&self) -> &str {
        "codex"
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
        if !config_path.exists() {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let content = fs::read_to_string(&config_path)?;
        let config = Self::parse_config_toml(&content)?;
        let notify_args = Self::notify_args_from_config(&config);
        let desired_notify = Self::desired_notify_args(&params.binary_path);

        let hooks_installed = notify_args
            .as_ref()
            .map(|args| Self::is_git_ai_codex_notify_args(args))
            .unwrap_or(false);
        let hooks_up_to_date = if hooks_installed {
            Self::apply_notify(&config, &desired_notify)? == config
        } else {
            false
        };

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

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let existing_content = if config_path.exists() {
            fs::read_to_string(&config_path)?
        } else {
            String::new()
        };

        let existing = Self::parse_config_toml(&existing_content)?;
        let desired_notify = Self::desired_notify_args(&params.binary_path);
        let merged = Self::apply_notify(&existing, &desired_notify)?;

        if existing == merged {
            return Ok(None);
        }

        let new_content = toml::to_string_pretty(&merged).map_err(|e| {
            GitAiError::Generic(format!("Failed to serialize Codex config.toml: {e}"))
        })?;
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
        let existing = Self::parse_config_toml(&existing_content)?;

        let Some(merged) = Self::remove_notify_if_git_ai(&existing)? else {
            return Ok(None);
        };

        let new_content = toml::to_string_pretty(&merged).map_err(|e| {
            GitAiError::Generic(format!("Failed to serialize Codex config.toml: {e}"))
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
    fn test_is_git_ai_codex_notify_args_false_for_non_git_ai_command() {
        let args = vec![
            "notify-send".to_string(),
            "Codex".to_string(),
            "done".to_string(),
        ];

        assert!(!CodexInstaller::is_git_ai_codex_notify_args(&args));
    }

    #[test]
    fn test_apply_notify_sets_notify_array() {
        let existing = CodexInstaller::parse_config_toml("model = \"gpt-5\"").unwrap();
        let desired = CodexInstaller::desired_notify_args(&test_binary_path());
        let merged = CodexInstaller::apply_notify(&existing, &desired).unwrap();

        let notify = CodexInstaller::notify_args_from_config(&merged).unwrap();
        assert_eq!(notify, desired);
        assert_eq!(
            merged.get("model").and_then(|v| v.as_str()),
            Some("gpt-5"),
            "Other config fields should be preserved"
        );
    }

    #[test]
    fn test_apply_notify_preserves_existing_git_ai_notify_extra_args() {
        let existing = CodexInstaller::parse_config_toml(
            r#"
notify = ["/usr/local/bin/git-ai", "checkpoint", "codex", "--hook-input", "afplay ~/Documents/celebration.wav"]
"#,
        )
        .unwrap();
        let desired = CodexInstaller::desired_notify_args(&test_binary_path());
        let merged = CodexInstaller::apply_notify(&existing, &desired).unwrap();

        let notify = CodexInstaller::notify_args_from_config(&merged).unwrap();
        assert_eq!(
            notify,
            vec![
                "/usr/local/bin/git-ai".to_string(),
                "checkpoint".to_string(),
                "codex".to_string(),
                "--hook-input".to_string(),
                "afplay ~/Documents/celebration.wav".to_string(),
            ]
        );
        assert_eq!(
            merged, existing,
            "already-merged notify should remain unchanged"
        );
    }

    #[test]
    fn test_apply_notify_updates_binary_path_and_preserves_extra_args() {
        let existing = CodexInstaller::parse_config_toml(
            r#"
notify = ["/tmp/git-ai", "checkpoint", "codex", "--hook-input", "--verbose"]
"#,
        )
        .unwrap();
        let desired = CodexInstaller::desired_notify_args(&test_binary_path());
        let merged = CodexInstaller::apply_notify(&existing, &desired).unwrap();

        let notify = CodexInstaller::notify_args_from_config(&merged).unwrap();
        assert_eq!(
            notify,
            vec![
                "/usr/local/bin/git-ai".to_string(),
                "checkpoint".to_string(),
                "codex".to_string(),
                "--hook-input".to_string(),
                "--verbose".to_string(),
            ]
        );
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
    #[serial]
    fn test_install_hooks_updates_config_and_check_reports_up_to_date() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
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
            let notify = CodexInstaller::notify_args_from_config(&parsed).unwrap();
            assert_eq!(
                notify,
                CodexInstaller::desired_notify_args(&params.binary_path)
            );

            let check = installer
                .check_hooks(&params)
                .expect("check should succeed");
            assert!(check.tool_installed);
            assert!(check.hooks_installed);
            assert!(check.hooks_up_to_date);
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
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_idempotent() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
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
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_preserves_git_ai_notify_extra_args() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
            fs::write(
                &config_path,
                r#"
model = "gpt-5"
notify = ["/usr/local/bin/git-ai", "checkpoint", "codex", "--hook-input", "afplay ~/Documents/celebration.wav"]
"#,
            )
            .unwrap();

            let installer = CodexInstaller;
            let params = HookInstallerParams {
                binary_path: test_binary_path(),
            };

            let result = installer
                .install_hooks(&params, false)
                .expect("install should succeed");
            assert!(
                result.is_none(),
                "install should not overwrite user notify extras when already up to date"
            );

            let content = fs::read_to_string(&config_path).unwrap();
            let parsed = CodexInstaller::parse_config_toml(&content).unwrap();
            let notify = CodexInstaller::notify_args_from_config(&parsed).unwrap();
            assert_eq!(
                notify,
                vec![
                    "/usr/local/bin/git-ai".to_string(),
                    "checkpoint".to_string(),
                    "codex".to_string(),
                    "--hook-input".to_string(),
                    "afplay ~/Documents/celebration.wav".to_string(),
                ]
            );
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_updates_binary_path_and_preserves_notify_extra_args() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
            fs::write(
                &config_path,
                r#"
model = "gpt-5"
notify = ["/tmp/git-ai", "checkpoint", "codex", "--hook-input", "--verbose"]
"#,
            )
            .unwrap();

            let installer = CodexInstaller;
            let params = HookInstallerParams {
                binary_path: test_binary_path(),
            };

            let result = installer
                .install_hooks(&params, false)
                .expect("install should succeed");
            assert!(
                result.is_some(),
                "install should update stale binary path while preserving user extras"
            );

            let content = fs::read_to_string(&config_path).unwrap();
            let parsed = CodexInstaller::parse_config_toml(&content).unwrap();
            let notify = CodexInstaller::notify_args_from_config(&parsed).unwrap();
            assert_eq!(
                notify,
                vec![
                    "/usr/local/bin/git-ai".to_string(),
                    "checkpoint".to_string(),
                    "codex".to_string(),
                    "--hook-input".to_string(),
                    "--verbose".to_string(),
                ]
            );
        });
    }

    #[test]
    #[serial]
    fn test_uninstall_hooks_removes_git_ai_notify_entry() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).unwrap();
            let config_path = codex_dir.join("config.toml");
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

            let diff = installer
                .uninstall_hooks(&params, false)
                .expect("uninstall should succeed");
            assert!(diff.is_some(), "uninstall should report a config diff");

            let content = fs::read_to_string(&config_path).unwrap();
            let parsed = CodexInstaller::parse_config_toml(&content).unwrap();
            assert!(
                CodexInstaller::notify_args_from_config(&parsed).is_none(),
                "notify should be removed"
            );
            assert_eq!(parsed.get("model").and_then(|v| v.as_str()), Some("gpt-5"));
        });
    }
}
