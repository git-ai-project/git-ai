use crate::error::GitAiError;
use crate::mdm::hook_installer::{
    HookCheckResult, HookInstaller, HookInstallerParams, InstallResult,
};
use crate::mdm::utils::{
    MIN_CURSOR_VERSION, generate_diff, get_editor_version, home_dir, install_vsc_editor_extension,
    is_vsc_editor_extension_installed, parse_version, resolve_editor_cli,
    settings_paths_for_products, should_process_settings_target, version_meets_requirement,
    write_atomic,
};
use crate::utils::debug_log;
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

// Command patterns for hooks
const CURSOR_BEFORE_SUBMIT_CMD: &str = "checkpoint cursor --hook-input stdin";
const CURSOR_AFTER_EDIT_CMD: &str = "checkpoint cursor --hook-input stdin";

pub struct CursorInstaller;

impl CursorInstaller {
    fn hooks_path() -> PathBuf {
        home_dir().join(".cursor").join("hooks.json")
    }

    fn settings_targets() -> Vec<PathBuf> {
        settings_paths_for_products(&["Cursor"])
    }

    fn is_cursor_checkpoint_command(cmd: &str) -> bool {
        cmd.contains("git-ai checkpoint cursor")
            || (cmd.contains("git-ai") && cmd.contains("checkpoint") && cmd.contains("cursor"))
    }
}

impl HookInstaller for CursorInstaller {
    fn name(&self) -> &str {
        "Cursor"
    }

    fn id(&self) -> &str {
        "cursor"
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let resolved_cli = resolve_editor_cli("cursor");
        let has_cli = resolved_cli.is_some();
        let has_dotfiles = home_dir().join(".cursor").exists();
        let has_settings_targets = Self::settings_targets()
            .iter()
            .any(|path| should_process_settings_target(path));

        if !has_cli && !has_dotfiles && !has_settings_targets {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        // If we have a CLI, check version
        if let Some(cli) = &resolved_cli
            && let Ok(version_str) = get_editor_version(cli)
            && let Some(version) = parse_version(&version_str)
            && !version_meets_requirement(version, MIN_CURSOR_VERSION)
        {
            return Err(GitAiError::Generic(format!(
                "Cursor version {}.{} detected, but minimum version {}.{} is required",
                version.0, version.1, MIN_CURSOR_VERSION.0, MIN_CURSOR_VERSION.1
            )));
        }

        // Check if hooks are installed
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

        let has_hooks = existing
            .get("hooks")
            .and_then(|h| h.get("beforeSubmitPrompt"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|hook| {
                    hook.get("command")
                        .and_then(|c| c.as_str())
                        .map(Self::is_cursor_checkpoint_command)
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
        let hooks_path = Self::hooks_path();

        // Ensure directory exists
        if let Some(dir) = hooks_path.parent() {
            fs::create_dir_all(dir)?;
        }

        // Read existing content as string
        let existing_content = if hooks_path.exists() {
            fs::read_to_string(&hooks_path)?
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
        let before_submit_cmd = format!(
            "{} {}",
            params.binary_path.display(),
            CURSOR_BEFORE_SUBMIT_CMD
        );
        let after_edit_cmd = format!("{} {}", params.binary_path.display(), CURSOR_AFTER_EDIT_CMD);

        // Desired hooks payload for Cursor
        let desired: Value = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    {
                        "command": before_submit_cmd
                    }
                ],
                "afterFileEdit": [
                    {
                        "command": after_edit_cmd
                    }
                ]
            }
        });

        // Merge desired into existing
        let mut merged = existing.clone();

        // Ensure version is set
        if merged.get("version").is_none()
            && let Some(obj) = merged.as_object_mut()
        {
            obj.insert("version".to_string(), json!(1));
        }

        // Merge hooks object
        let mut hooks_obj = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));

        // Process both hook types
        for hook_name in &["beforeSubmitPrompt", "afterFileEdit"] {
            let desired_hooks = desired
                .get("hooks")
                .and_then(|h| h.get(*hook_name))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Get existing hooks array for this hook type
            let mut existing_hooks = hooks_obj
                .get(*hook_name)
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Update outdated git-ai checkpoint commands (or add if missing)
            for desired_hook in desired_hooks {
                let desired_cmd = desired_hook.get("command").and_then(|c| c.as_str());
                if desired_cmd.is_none() {
                    continue;
                }
                let desired_cmd = desired_cmd.unwrap();

                // Look for existing git-ai checkpoint cursor commands
                let mut found_idx = None;
                let mut needs_update = false;

                for (idx, existing_hook) in existing_hooks.iter().enumerate() {
                    if let Some(existing_cmd) =
                        existing_hook.get("command").and_then(|c| c.as_str())
                        && Self::is_cursor_checkpoint_command(existing_cmd)
                    {
                        found_idx = Some(idx);
                        if existing_cmd != desired_cmd {
                            needs_update = true;
                        }
                        break;
                    }
                }

                match found_idx {
                    Some(idx) if needs_update => {
                        existing_hooks[idx] = desired_hook.clone();
                    }
                    Some(_) => {
                        // Already up to date, skip
                    }
                    None => {
                        // No existing command, add new one
                        existing_hooks.push(desired_hook.clone());
                    }
                }
            }

            // Write back merged hooks for this hook type
            if let Some(obj) = hooks_obj.as_object_mut() {
                obj.insert(hook_name.to_string(), Value::Array(existing_hooks));
            }
        }

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
        let diff_output = generate_diff(&hooks_path, &existing_content, &new_content);

        // Write if not dry-run
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

        // Remove git-ai checkpoint cursor commands from both hook types
        for hook_name in &["beforeSubmitPrompt", "afterFileEdit"] {
            if let Some(hooks_array) = hooks_obj.get_mut(*hook_name).and_then(|v| v.as_array_mut())
            {
                let original_len = hooks_array.len();
                hooks_array.retain(|hook| {
                    if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                        !Self::is_cursor_checkpoint_command(cmd)
                    } else {
                        true
                    }
                });
                if hooks_array.len() != original_len {
                    changed = true;
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
        let diff_output = generate_diff(&hooks_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(&hooks_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }

    fn install_extras(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Vec<InstallResult>, GitAiError> {
        let mut results = Vec::new();

        // Install VS Code extension
        if let Some(cli) = resolve_editor_cli("cursor") {
            match is_vsc_editor_extension_installed(&cli, "git-ai.git-ai-vscode") {
                Ok(true) => {
                    results.push(InstallResult {
                        changed: false,
                        diff: None,
                        message: "Cursor: Extension already installed".to_string(),
                    });
                }
                Ok(false) => {
                    if dry_run {
                        results.push(InstallResult {
                            changed: true,
                            diff: None,
                            message: "Cursor: Pending extension install".to_string(),
                        });
                    } else {
                        println!("Installing extensions...");
                        println!("\tInstalling extension 'git-ai.git-ai-vscode'...");
                        match install_vsc_editor_extension(&cli, "git-ai.git-ai-vscode") {
                            Ok(()) => {
                                results.push(InstallResult {
                                    changed: true,
                                    diff: None,
                                    message: "\tExtension 'git-ai.git-ai-vscode' was successfully installed.".to_string(),
                                });
                            }
                            Err(e) => {
                                debug_log(&format!(
                                    "Cursor: Error automatically installing extension: {}",
                                    e
                                ));
                                results.push(InstallResult {
                                    changed: false,
                                    diff: None,
                                    message: "Cursor: Unable to automatically install extension. Please cmd+click on the following link to install: cursor:extension/git-ai.git-ai-vscode (or search for 'git-ai-vscode' in the Cursor extensions tab)".to_string(),
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    results.push(InstallResult {
                        changed: false,
                        diff: None,
                        message: format!("Cursor: Failed to check extension: {}", e),
                    });
                }
            }
        } else {
            results.push(InstallResult {
                changed: false,
                diff: None,
                message: "Cursor: Unable to automatically install extension. Please cmd+click on the following link to install: cursor:extension/git-ai.git-ai-vscode (or search for 'git-ai-vscode' in the Cursor extensions tab)".to_string(),
            });
        }

        // Configure git.path
        {
            use crate::mdm::utils::{git_shim_path_string, update_git_path_setting};

            let git_path = git_shim_path_string();
            for settings_path in Self::settings_targets() {
                if !should_process_settings_target(&settings_path) {
                    continue;
                }

                match update_git_path_setting(&settings_path, &git_path, dry_run) {
                    Ok(Some(diff)) => {
                        results.push(InstallResult {
                            changed: true,
                            diff: Some(diff),
                            message: format!(
                                "Cursor: git.path updated in {}",
                                settings_path.display()
                            ),
                        });
                    }
                    Ok(None) => {
                        results.push(InstallResult {
                            changed: false,
                            diff: None,
                            message: format!(
                                "Cursor: git.path already configured in {}",
                                settings_path.display()
                            ),
                        });
                    }
                    Err(e) => {
                        results.push(InstallResult {
                            changed: false,
                            diff: None,
                            message: format!("Cursor: Failed to configure git.path: {}", e),
                        });
                    }
                }
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdm::utils::clean_path;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_env() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let hooks_path = temp_dir.path().join(".cursor").join("hooks.json");
        (temp_dir, hooks_path)
    }

    fn create_test_binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    #[test]
    fn test_install_hooks_creates_file_from_scratch() {
        let (_temp_dir, hooks_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        if let Some(parent) = hooks_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let git_ai_cmd = format!("{} {}", binary_path.display(), CURSOR_BEFORE_SUBMIT_CMD);

        let result = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    {
                        "command": git_ai_cmd.clone()
                    }
                ],
                "afterFileEdit": [
                    {
                        "command": git_ai_cmd.clone()
                    }
                ]
            }
        });

        let pretty = serde_json::to_string_pretty(&result).unwrap();
        fs::write(&hooks_path, pretty).unwrap();

        assert!(hooks_path.exists());

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        assert_eq!(content.get("version").unwrap(), &json!(1));

        let hooks = content.get("hooks").unwrap();
        let before_submit = hooks.get("beforeSubmitPrompt").unwrap().as_array().unwrap();
        let after_edit = hooks.get("afterFileEdit").unwrap().as_array().unwrap();

        assert_eq!(before_submit.len(), 1);
        assert_eq!(after_edit.len(), 1);
        assert!(
            before_submit[0]
                .get("command")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("git-ai checkpoint cursor")
        );
    }

    #[test]
    fn test_install_hooks_preserves_existing_hooks() {
        let (_temp_dir, hooks_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        if let Some(parent) = hooks_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let existing = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    {
                        "command": "echo 'before'"
                    }
                ],
                "afterFileEdit": [
                    {
                        "command": "echo 'after'"
                    }
                ]
            }
        });
        fs::write(
            &hooks_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let git_ai_cmd = format!("{} {}", binary_path.display(), CURSOR_BEFORE_SUBMIT_CMD);

        let mut content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();

        for hook_name in &["beforeSubmitPrompt", "afterFileEdit"] {
            let hooks_obj = content.get_mut("hooks").unwrap();
            let mut hooks_array = hooks_obj
                .get(*hook_name)
                .unwrap()
                .as_array()
                .unwrap()
                .clone();
            hooks_array.push(json!({"command": git_ai_cmd.clone()}));
            hooks_obj
                .as_object_mut()
                .unwrap()
                .insert(hook_name.to_string(), Value::Array(hooks_array));
        }

        fs::write(&hooks_path, serde_json::to_string_pretty(&content).unwrap()).unwrap();

        let result: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let hooks = result.get("hooks").unwrap();

        let before_submit = hooks.get("beforeSubmitPrompt").unwrap().as_array().unwrap();
        let after_edit = hooks.get("afterFileEdit").unwrap().as_array().unwrap();

        assert_eq!(before_submit.len(), 2);
        assert_eq!(after_edit.len(), 2);

        assert_eq!(
            before_submit[0].get("command").unwrap().as_str().unwrap(),
            "echo 'before'"
        );
        assert_eq!(
            after_edit[0].get("command").unwrap().as_str().unwrap(),
            "echo 'after'"
        );
    }

    #[test]
    fn test_install_hooks_updates_outdated_command() {
        let (_temp_dir, hooks_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        if let Some(parent) = hooks_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let existing = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    {
                        "command": "git-ai checkpoint cursor 2>/dev/null || true"
                    }
                ],
                "afterFileEdit": [
                    {
                        "command": "/old/path/git-ai checkpoint cursor"
                    }
                ]
            }
        });
        fs::write(
            &hooks_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let git_ai_cmd = format!("{} {}", binary_path.display(), CURSOR_BEFORE_SUBMIT_CMD);

        let mut content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();

        for hook_name in &["beforeSubmitPrompt", "afterFileEdit"] {
            let hooks_obj = content.get_mut("hooks").unwrap();
            let mut hooks_array = hooks_obj
                .get(*hook_name)
                .unwrap()
                .as_array()
                .unwrap()
                .clone();

            for hook in hooks_array.iter_mut() {
                if let Some(cmd) = hook.get("command").and_then(|c| c.as_str())
                    && CursorInstaller::is_cursor_checkpoint_command(cmd)
                {
                    *hook = json!({"command": git_ai_cmd.clone()});
                }
            }

            hooks_obj
                .as_object_mut()
                .unwrap()
                .insert(hook_name.to_string(), Value::Array(hooks_array));
        }

        fs::write(&hooks_path, serde_json::to_string_pretty(&content).unwrap()).unwrap();

        let result: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let hooks = result.get("hooks").unwrap();

        let before_submit = hooks.get("beforeSubmitPrompt").unwrap().as_array().unwrap();
        let after_edit = hooks.get("afterFileEdit").unwrap().as_array().unwrap();

        assert_eq!(before_submit.len(), 1);
        assert_eq!(after_edit.len(), 1);

        assert_eq!(
            before_submit[0].get("command").unwrap().as_str().unwrap(),
            git_ai_cmd
        );
        assert_eq!(
            after_edit[0].get("command").unwrap().as_str().unwrap(),
            git_ai_cmd
        );
    }

    #[test]
    fn test_cursor_hook_commands_no_windows_extended_path_prefix() {
        let raw_path = PathBuf::from(r"\\?\C:\Users\USERNAME\.git-ai\bin\git-ai.exe");
        let binary_path = clean_path(raw_path);

        let before_submit_cmd = format!("{} {}", binary_path.display(), CURSOR_BEFORE_SUBMIT_CMD);
        let after_edit_cmd = format!("{} {}", binary_path.display(), CURSOR_AFTER_EDIT_CMD);

        assert!(
            !before_submit_cmd.contains(r"\\?\"),
            "beforeSubmitPrompt command should not contain \\\\?\\ prefix, got: {}",
            before_submit_cmd
        );
        assert!(
            !after_edit_cmd.contains(r"\\?\"),
            "afterFileEdit command should not contain \\\\?\\ prefix, got: {}",
            after_edit_cmd
        );
        assert!(
            before_submit_cmd.contains("checkpoint cursor"),
            "command should still contain checkpoint args"
        );
    }

    #[test]
    fn test_cursor_settings_targets_returns_candidates() {
        let targets = CursorInstaller::settings_targets();
        assert!(!targets.is_empty());
    }
}
