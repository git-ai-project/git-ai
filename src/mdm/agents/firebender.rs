use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{generate_diff, home_dir, write_atomic};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

const FIREBENDER_CHECKPOINT_CMD: &str = "checkpoint firebender --hook-input stdin";
const FIREBENDER_PRE_TOOL_USE_CMD: &str = "checkpoint firebender --hook-input stdin";
const FIREBENDER_POST_TOOL_USE_CMD: &str = "checkpoint firebender --hook-input stdin";

pub struct FirebenderInstaller;

impl FirebenderInstaller {
    fn hooks_path() -> PathBuf {
        home_dir().join(".firebender").join("hooks.json")
    }

    fn is_firebender_checkpoint_command(cmd: &str) -> bool {
        cmd.contains("checkpoint firebender")
            && (cmd.contains("git-ai") || cmd.ends_with(FIREBENDER_CHECKPOINT_CMD))
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

        let has_pre = existing
            .get("hooks")
            .and_then(|h| h.get("preToolUse"))
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

        let has_post = existing
            .get("hooks")
            .and_then(|h| h.get("postToolUse"))
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
            hooks_installed: has_pre && has_post,
            hooks_up_to_date: has_pre && has_post,
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

        let pre_tool_use_cmd = format!(
            "{} {}",
            params.binary_path.display(),
            FIREBENDER_PRE_TOOL_USE_CMD
        );
        let post_tool_use_cmd = format!(
            "{} {}",
            params.binary_path.display(),
            FIREBENDER_POST_TOOL_USE_CMD
        );

        let desired: Value = json!({
            "version": 1,
            "hooks": {
                "preToolUse": [
                    {
                        "command": pre_tool_use_cmd
                    }
                ],
                "postToolUse": [
                    {
                        "command": post_tool_use_cmd
                    }
                ]
            }
        });

        let mut merged = existing.clone();
        if merged.get("version").is_none()
            && let Some(obj) = merged.as_object_mut()
        {
            obj.insert("version".to_string(), json!(1));
        }

        let mut hooks_obj = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));

        for hook_name in &["preToolUse", "postToolUse"] {
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
                    if let Some(existing_cmd) =
                        existing_hook.get("command").and_then(|c| c.as_str())
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
        let mut hooks_obj = match merged.get("hooks").cloned() {
            Some(h) => h,
            None => return Ok(None),
        };
        let mut changed = false;

        for hook_name in &["preToolUse", "postToolUse"] {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::path::Path;
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
    fn test_firebender_command_detection() {
        assert!(FirebenderInstaller::is_firebender_checkpoint_command(
            "/usr/local/bin/git-ai checkpoint firebender --hook-input stdin"
        ));
        assert!(FirebenderInstaller::is_firebender_checkpoint_command(
            "git-ai something checkpoint firebender"
        ));
        assert!(!FirebenderInstaller::is_firebender_checkpoint_command(
            "git-ai checkpoint cursor"
        ));
    }

    #[test]
    #[serial]
    fn test_install_hooks_creates_expected_entries() {
        with_temp_home(|home| {
            let hooks_path = home.join(".firebender").join("hooks.json");
            let installer = FirebenderInstaller;
            let diff = installer
                .install_hooks(
                    &HookInstallerParams {
                        binary_path: create_test_binary_path(),
                    },
                    false,
                )
                .unwrap();

            assert!(diff.is_some());

            let content: Value =
                serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
            assert_eq!(content.get("version").unwrap(), &json!(1));
            assert!(
                content["hooks"]["preToolUse"][0]["command"]
                    .as_str()
                    .unwrap()
                    .contains("checkpoint firebender")
            );
            assert!(
                content["hooks"]["postToolUse"][0]["command"]
                    .as_str()
                    .unwrap()
                    .contains("checkpoint firebender")
            );
        });
    }

    #[test]
    #[serial]
    fn test_install_hooks_updates_existing_firebender_command() {
        with_temp_home(|home| {
            let hooks_path = home.join(".firebender").join("hooks.json");
            if let Some(parent) = hooks_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }

            let existing = json!({
                "version": 1,
                "hooks": {
                    "preToolUse": [{ "command": "/old/path/git-ai checkpoint firebender --hook-input stdin" }],
                    "postToolUse": [{ "command": "/old/path/git-ai checkpoint firebender --hook-input stdin" }]
                }
            });
            fs::write(
                &hooks_path,
                serde_json::to_string_pretty(&existing).unwrap(),
            )
            .unwrap();

            let installer = FirebenderInstaller;
            let diff = installer
                .install_hooks(
                    &HookInstallerParams {
                        binary_path: create_test_binary_path(),
                    },
                    false,
                )
                .unwrap();

            assert!(diff.is_some());

            let updated: Value =
                serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
            let expected = format!(
                "{} {}",
                create_test_binary_path().display(),
                FIREBENDER_CHECKPOINT_CMD
            );
            assert_eq!(
                updated["hooks"]["preToolUse"][0]["command"]
                    .as_str()
                    .unwrap(),
                expected
            );
            assert_eq!(
                updated["hooks"]["postToolUse"][0]["command"]
                    .as_str()
                    .unwrap(),
                expected
            );
        });
    }

    #[test]
    #[serial]
    fn test_uninstall_hooks_removes_only_firebender_entries() {
        with_temp_home(|home| {
            let hooks_path = home.join(".firebender").join("hooks.json");
            if let Some(parent) = hooks_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }

            let existing = json!({
                "version": 1,
                "hooks": {
                    "preToolUse": [
                        { "command": "/usr/local/bin/git-ai checkpoint firebender --hook-input stdin" },
                        { "command": "echo keep-before" }
                    ],
                    "postToolUse": [
                        { "command": "/usr/local/bin/git-ai checkpoint firebender --hook-input stdin" },
                        { "command": "echo keep-after" }
                    ]
                }
            });
            fs::write(
                &hooks_path,
                serde_json::to_string_pretty(&existing).unwrap(),
            )
            .unwrap();

            let installer = FirebenderInstaller;
            let diff = installer
                .uninstall_hooks(
                    &HookInstallerParams {
                        binary_path: create_test_binary_path(),
                    },
                    false,
                )
                .unwrap();

            assert!(diff.is_some());

            let updated: Value =
                serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
            assert_eq!(updated["hooks"]["preToolUse"].as_array().unwrap().len(), 1);
            assert_eq!(updated["hooks"]["postToolUse"].as_array().unwrap().len(), 1);
            assert_eq!(
                updated["hooks"]["preToolUse"][0]["command"]
                    .as_str()
                    .unwrap(),
                "echo keep-before"
            );
            assert_eq!(
                updated["hooks"]["postToolUse"][0]["command"]
                    .as_str()
                    .unwrap(),
                "echo keep-after"
            );
        });
    }
}
