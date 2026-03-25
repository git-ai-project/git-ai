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
const CURSOR_PRE_TOOL_USE_CMD: &str = "checkpoint cursor --hook-input stdin";
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

        let (hooks_installed, hooks_up_to_date) = Self::check_hooks_file(&hooks_path)?;
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
        Self::install_hooks_at(&Self::hooks_path(), params, dry_run)
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        Self::uninstall_hooks_at(&Self::hooks_path(), dry_run)
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

impl CursorInstaller {
    /// Returns `(hooks_installed, hooks_up_to_date)` by inspecting the hooks file.
    ///
    /// - `hooks_installed`: true if any git-ai checkpoint command is present in any hook type.
    /// - `hooks_up_to_date`: true only if the `preToolUse` hook contains a git-ai checkpoint command.
    ///   A file with only the legacy `beforeSubmitPrompt` hook returns `(true, false)` so that
    ///   `install_hooks` will migrate it to `preToolUse`.
    fn check_hooks_file(hooks_path: &PathBuf) -> Result<(bool, bool), GitAiError> {
        let content = fs::read_to_string(hooks_path)?;
        let existing: Value = serde_json::from_str(&content).unwrap_or_else(|_| json!({}));
        let hooks = existing.get("hooks");

        let has_pre_tool_use = hooks
            .and_then(|h| h.get("preToolUse"))
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

        // Legacy: beforeSubmitPrompt-only installs are recognised as present but stale.
        let has_legacy_before_submit = hooks
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

        Ok((
            has_pre_tool_use || has_legacy_before_submit,
            has_pre_tool_use,
        ))
    }

    /// Core install logic. Extracted so tests can supply a custom path without touching the
    /// real `~/.cursor/hooks.json`.
    fn install_hooks_at(
        hooks_path: &PathBuf,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        // Ensure directory exists
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

        let pre_tool_use_cmd = format!(
            "{} {}",
            params.binary_path.display(),
            CURSOR_PRE_TOOL_USE_CMD
        );
        let after_edit_cmd = format!("{} {}", params.binary_path.display(), CURSOR_AFTER_EDIT_CMD);

        let desired: Value = json!({
            "version": 1,
            "hooks": {
                "preToolUse": [{ "command": pre_tool_use_cmd }],
                "afterFileEdit": [{ "command": after_edit_cmd }]
            }
        });

        let mut merged = existing.clone();

        if merged.get("version").is_none()
            && let Some(obj) = merged.as_object_mut()
        {
            obj.insert("version".to_string(), json!(1));
        }

        let mut hooks_obj = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));

        // Migration: remove any legacy beforeSubmitPrompt git-ai entries.
        if let Some(before_submit_arr) = hooks_obj
            .get_mut("beforeSubmitPrompt")
            .and_then(|v| v.as_array_mut())
        {
            before_submit_arr.retain(|hook| {
                hook.get("command")
                    .and_then(|c| c.as_str())
                    .map(|cmd| !Self::is_cursor_checkpoint_command(cmd))
                    .unwrap_or(true)
            });
        }

        for hook_name in &["preToolUse", "afterFileEdit"] {
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
                let desired_cmd = match desired_hook.get("command").and_then(|c| c.as_str()) {
                    Some(c) => c,
                    None => continue,
                };

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
        let diff_output = generate_diff(hooks_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(hooks_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }

    /// Core uninstall logic. Extracted so tests can supply a custom path.
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

        // Remove git-ai checkpoint cursor commands from all hook types, including the legacy
        // beforeSubmitPrompt so existing users are fully cleaned up.
        for hook_name in &["preToolUse", "beforeSubmitPrompt", "afterFileEdit"] {
            if let Some(hooks_array) = hooks_obj.get_mut(*hook_name).and_then(|v| v.as_array_mut())
            {
                let original_len = hooks_array.len();
                hooks_array.retain(|hook| {
                    hook.get("command")
                        .and_then(|c| c.as_str())
                        .map(|cmd| !Self::is_cursor_checkpoint_command(cmd))
                        .unwrap_or(true)
                });
                if hooks_array.len() != original_len {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdm::hook_installer::HookInstallerParams;
    use crate::mdm::utils::clean_path;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_env() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let hooks_path = temp_dir.path().join(".cursor").join("hooks.json");
        fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
        (temp_dir, hooks_path)
    }

    fn make_params(binary: &str) -> HookInstallerParams {
        HookInstallerParams {
            binary_path: PathBuf::from(binary),
        }
    }

    // ──────────────────────────────────────────────────────────────────────────
    // install_hooks_at
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_install_hooks_creates_file_from_scratch() {
        let (_tmp, hooks_path) = setup_test_env();
        let params = make_params("/usr/local/bin/git-ai");

        let diff = CursorInstaller::install_hooks_at(&hooks_path, &params, false)
            .expect("install should succeed");
        assert!(
            diff.is_some(),
            "should report a change when creating from scratch"
        );

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        assert_eq!(content["version"], json!(1));

        let hooks = &content["hooks"];
        let pre_tool_use = hooks["preToolUse"].as_array().unwrap();
        let after_edit = hooks["afterFileEdit"].as_array().unwrap();

        assert_eq!(pre_tool_use.len(), 1);
        assert_eq!(after_edit.len(), 1);
        assert!(
            pre_tool_use[0]["command"]
                .as_str()
                .unwrap()
                .contains("git-ai checkpoint cursor"),
            "preToolUse command should contain checkpoint args"
        );
        assert!(
            !hooks.get("beforeSubmitPrompt").is_some_and(|v| {
                v.as_array().unwrap_or(&vec![]).iter().any(|h| {
                    CursorInstaller::is_cursor_checkpoint_command(
                        h["command"].as_str().unwrap_or(""),
                    )
                })
            }),
            "should not install a beforeSubmitPrompt git-ai hook"
        );
    }

    #[test]
    fn test_install_hooks_preserves_non_gitai_hooks() {
        let (_tmp, hooks_path) = setup_test_env();
        // Write existing hooks from other tools
        let existing = json!({
            "version": 1,
            "hooks": {
                "preToolUse": [{ "command": "echo 'other tool pre'" }],
                "afterFileEdit": [{ "command": "echo 'other tool after'" }]
            }
        });
        fs::write(
            &hooks_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let params = make_params("/usr/local/bin/git-ai");
        CursorInstaller::install_hooks_at(&hooks_path, &params, false).unwrap();

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let pre_tool_use = content["hooks"]["preToolUse"].as_array().unwrap();
        let after_edit = content["hooks"]["afterFileEdit"].as_array().unwrap();

        // Both arrays should have the existing hook plus the new git-ai hook
        assert_eq!(pre_tool_use.len(), 2);
        assert_eq!(after_edit.len(), 2);
        assert_eq!(
            pre_tool_use[0]["command"].as_str().unwrap(),
            "echo 'other tool pre'"
        );
        assert_eq!(
            after_edit[0]["command"].as_str().unwrap(),
            "echo 'other tool after'"
        );
    }

    #[test]
    fn test_install_hooks_updates_outdated_path() {
        let (_tmp, hooks_path) = setup_test_env();
        // Simulate an install with an old binary path
        let existing = json!({
            "version": 1,
            "hooks": {
                "preToolUse": [{ "command": "/old/path/git-ai checkpoint cursor --hook-input stdin" }],
                "afterFileEdit": [{ "command": "/old/path/git-ai checkpoint cursor --hook-input stdin" }]
            }
        });
        fs::write(
            &hooks_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let params = make_params("/new/path/git-ai");
        CursorInstaller::install_hooks_at(&hooks_path, &params, false).unwrap();

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let pre_tool_use = content["hooks"]["preToolUse"].as_array().unwrap();
        let after_edit = content["hooks"]["afterFileEdit"].as_array().unwrap();

        assert_eq!(pre_tool_use.len(), 1, "should not duplicate; only update");
        assert_eq!(
            pre_tool_use[0]["command"].as_str().unwrap(),
            "/new/path/git-ai checkpoint cursor --hook-input stdin"
        );
        assert_eq!(after_edit.len(), 1);
        assert_eq!(
            after_edit[0]["command"].as_str().unwrap(),
            "/new/path/git-ai checkpoint cursor --hook-input stdin"
        );
    }

    #[test]
    fn test_install_hooks_is_idempotent() {
        let (_tmp, hooks_path) = setup_test_env();
        let params = make_params("/usr/local/bin/git-ai");

        CursorInstaller::install_hooks_at(&hooks_path, &params, false).unwrap();
        let diff2 = CursorInstaller::install_hooks_at(&hooks_path, &params, false).unwrap();
        assert!(diff2.is_none(), "second install should report no change");
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Migration: beforeSubmitPrompt → preToolUse
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_check_hooks_file_legacy_before_submit_is_installed_not_up_to_date() {
        let (_tmp, hooks_path) = setup_test_env();
        // State: user has the old beforeSubmitPrompt hook (no preToolUse yet)
        let legacy = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    { "command": "/usr/local/bin/git-ai checkpoint cursor --hook-input stdin" }
                ],
                "afterFileEdit": [
                    { "command": "/usr/local/bin/git-ai checkpoint cursor --hook-input stdin" }
                ]
            }
        });
        fs::write(&hooks_path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        let (installed, up_to_date) = CursorInstaller::check_hooks_file(&hooks_path).unwrap();
        assert!(
            installed,
            "legacy beforeSubmitPrompt hook should count as installed"
        );
        assert!(
            !up_to_date,
            "legacy hook should NOT be considered up-to-date"
        );
    }

    #[test]
    fn test_check_hooks_file_pre_tool_use_is_up_to_date() {
        let (_tmp, hooks_path) = setup_test_env();
        let modern = json!({
            "version": 1,
            "hooks": {
                "preToolUse": [
                    { "command": "/usr/local/bin/git-ai checkpoint cursor --hook-input stdin" }
                ],
                "afterFileEdit": [
                    { "command": "/usr/local/bin/git-ai checkpoint cursor --hook-input stdin" }
                ]
            }
        });
        fs::write(&hooks_path, serde_json::to_string_pretty(&modern).unwrap()).unwrap();

        let (installed, up_to_date) = CursorInstaller::check_hooks_file(&hooks_path).unwrap();
        assert!(installed);
        assert!(up_to_date);
    }

    #[test]
    fn test_install_hooks_migrates_from_before_submit_to_pre_tool_use() {
        let (_tmp, hooks_path) = setup_test_env();
        // State: existing user with old beforeSubmitPrompt git-ai hook (and a non-git-ai hook too)
        let legacy = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    { "command": "echo 'third-party hook'" },
                    { "command": "/usr/local/bin/git-ai checkpoint cursor --hook-input stdin" }
                ],
                "afterFileEdit": [
                    { "command": "/usr/local/bin/git-ai checkpoint cursor --hook-input stdin" }
                ]
            }
        });
        fs::write(&hooks_path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        let params = make_params("/usr/local/bin/git-ai");
        let diff = CursorInstaller::install_hooks_at(&hooks_path, &params, false).unwrap();
        assert!(diff.is_some(), "migration should report a change");

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let hooks = &content["hooks"];

        // git-ai entry must be REMOVED from beforeSubmitPrompt
        let before_submit = hooks["beforeSubmitPrompt"].as_array().unwrap();
        assert_eq!(
            before_submit.len(),
            1,
            "only the third-party hook should remain in beforeSubmitPrompt"
        );
        assert_eq!(
            before_submit[0]["command"].as_str().unwrap(),
            "echo 'third-party hook'"
        );

        // git-ai entry must be PRESENT in preToolUse
        let pre_tool_use = hooks["preToolUse"].as_array().unwrap();
        assert_eq!(pre_tool_use.len(), 1);
        assert!(
            CursorInstaller::is_cursor_checkpoint_command(
                pre_tool_use[0]["command"].as_str().unwrap()
            ),
            "preToolUse should contain the git-ai checkpoint command"
        );

        // afterFileEdit should be unchanged
        let after_edit = hooks["afterFileEdit"].as_array().unwrap();
        assert_eq!(after_edit.len(), 1);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // uninstall_hooks_at
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_uninstall_hooks_removes_pre_tool_use_and_after_edit() {
        let (_tmp, hooks_path) = setup_test_env();
        let params = make_params("/usr/local/bin/git-ai");
        CursorInstaller::install_hooks_at(&hooks_path, &params, false).unwrap();

        let diff = CursorInstaller::uninstall_hooks_at(&hooks_path, false).unwrap();
        assert!(diff.is_some(), "uninstall should report a change");

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let has_git_ai = |arr: &Value| {
            arr.as_array().unwrap_or(&vec![]).iter().any(|h| {
                CursorInstaller::is_cursor_checkpoint_command(h["command"].as_str().unwrap_or(""))
            })
        };
        assert!(!has_git_ai(&content["hooks"]["preToolUse"]));
        assert!(!has_git_ai(&content["hooks"]["afterFileEdit"]));
    }

    #[test]
    fn test_uninstall_hooks_also_removes_legacy_before_submit_prompt() {
        let (_tmp, hooks_path) = setup_test_env();
        // Simulate existing user who still has the old beforeSubmitPrompt hook
        let legacy = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    { "command": "echo 'other'" },
                    { "command": "/usr/local/bin/git-ai checkpoint cursor --hook-input stdin" }
                ],
                "afterFileEdit": [
                    { "command": "/usr/local/bin/git-ai checkpoint cursor --hook-input stdin" }
                ]
            }
        });
        fs::write(&hooks_path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        let diff = CursorInstaller::uninstall_hooks_at(&hooks_path, false).unwrap();
        assert!(diff.is_some());

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();

        // git-ai entry gone from beforeSubmitPrompt
        let before_submit = content["hooks"]["beforeSubmitPrompt"].as_array().unwrap();
        assert_eq!(
            before_submit.len(),
            1,
            "third-party hook should be preserved"
        );
        assert_eq!(
            before_submit[0]["command"].as_str().unwrap(),
            "echo 'other'"
        );

        // afterFileEdit git-ai entry gone
        let after_edit = content["hooks"]["afterFileEdit"].as_array().unwrap();
        assert!(
            !after_edit
                .iter()
                .any(|h| CursorInstaller::is_cursor_checkpoint_command(
                    h["command"].as_str().unwrap_or("")
                )),
            "afterFileEdit git-ai hook should be removed"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Miscellaneous
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_cursor_hook_commands_no_windows_extended_path_prefix() {
        let raw_path = PathBuf::from(r"\\?\C:\Users\USERNAME\.git-ai\bin\git-ai.exe");
        let binary_path = clean_path(raw_path);

        let pre_tool_use_cmd = format!("{} {}", binary_path.display(), CURSOR_PRE_TOOL_USE_CMD);
        let after_edit_cmd = format!("{} {}", binary_path.display(), CURSOR_AFTER_EDIT_CMD);

        assert!(
            !pre_tool_use_cmd.contains(r"\\?\"),
            "preToolUse command should not contain \\\\?\\ prefix, got: {}",
            pre_tool_use_cmd
        );
        assert!(
            !after_edit_cmd.contains(r"\\?\"),
            "afterFileEdit command should not contain \\\\?\\ prefix, got: {}",
            after_edit_cmd
        );
        assert!(
            pre_tool_use_cmd.contains("checkpoint cursor"),
            "command should still contain checkpoint args"
        );
    }

    #[test]
    fn test_cursor_settings_targets_returns_candidates() {
        let targets = CursorInstaller::settings_targets();
        assert!(!targets.is_empty());
    }
}
