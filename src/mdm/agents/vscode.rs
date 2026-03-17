use crate::error::GitAiError;
use crate::mdm::hook_installer::{
    HookCheckResult, HookInstaller, HookInstallerParams, InstallResult, UninstallResult,
};
use crate::mdm::utils::{
    MIN_CODE_VERSION, get_editor_version, home_dir, install_vsc_editor_extension,
    is_github_codespaces, is_vsc_editor_extension_installed, parse_version, resolve_editor_cli,
    settings_paths_for_products, should_process_settings_target, version_meets_requirement,
};
use crate::utils::debug_log;
use std::path::PathBuf;

pub struct VSCodeInstaller;

impl VSCodeInstaller {
    fn settings_targets() -> Vec<PathBuf> {
        settings_paths_for_products(&["Code", "Code - Insiders"])
    }
}

impl HookInstaller for VSCodeInstaller {
    fn name(&self) -> &str {
        "VS Code"
    }

    fn id(&self) -> &str {
        "vscode"
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let resolved_cli = resolve_editor_cli("code");
        let has_cli = resolved_cli.is_some();
        let has_dotfiles = home_dir().join(".vscode").exists();
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
            && !version_meets_requirement(version, MIN_CODE_VERSION)
        {
            return Err(GitAiError::Generic(format!(
                "VS Code version {}.{} detected, but minimum version {}.{} is required",
                version.0, version.1, MIN_CODE_VERSION.0, MIN_CODE_VERSION.1
            )));
        }

        // VS Code hooks are installed via extension, not config files
        // Check if extension is installed
        if let Some(cli) = &resolved_cli {
            match is_vsc_editor_extension_installed(cli, "git-ai.git-ai-vscode") {
                Ok(true) => {
                    return Ok(HookCheckResult {
                        tool_installed: true,
                        hooks_installed: true,
                        hooks_up_to_date: true,
                    });
                }
                Ok(false) | Err(_) => {
                    return Ok(HookCheckResult {
                        tool_installed: true,
                        hooks_installed: false,
                        hooks_up_to_date: false,
                    });
                }
            }
        }

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed: false,
            hooks_up_to_date: false,
        })
    }

    fn install_hooks(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        // VS Code doesn't have config file hooks, only extension
        // The install_extras method handles the extension installation
        Ok(None)
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        // VS Code doesn't have config file hooks to uninstall
        // The extension must be uninstalled manually through the editor
        Ok(None)
    }

    fn install_extras(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Vec<InstallResult>, GitAiError> {
        let mut results = Vec::new();

        // Skip extension installation in GitHub Codespaces
        // Extensions must be configured via devcontainer.json in Codespaces
        if is_github_codespaces() {
            results.push(InstallResult {
                changed: false,
                diff: None,
                message: "VS Code: Unable to install extension in GitHub Codespaces. Add to your devcontainer.json: \"customizations\": { \"vscode\": { \"extensions\": [\"git-ai.git-ai-vscode\"] } }".to_string(),
            });
            return Ok(results);
        }

        // Install VS Code extension
        if let Some(cli) = resolve_editor_cli("code") {
            match is_vsc_editor_extension_installed(&cli, "git-ai.git-ai-vscode") {
                Ok(true) => {
                    results.push(InstallResult {
                        changed: false,
                        diff: None,
                        message: "VS Code: Extension already installed".to_string(),
                    });
                }
                Ok(false) => {
                    if dry_run {
                        results.push(InstallResult {
                            changed: true,
                            diff: None,
                            message: "VS Code: Pending extension install".to_string(),
                        });
                    } else {
                        match install_vsc_editor_extension(&cli, "git-ai.git-ai-vscode") {
                            Ok(()) => {
                                results.push(InstallResult {
                                    changed: true,
                                    diff: None,
                                    message: "VS Code: Extension installed".to_string(),
                                });
                            }
                            Err(e) => {
                                debug_log(&format!(
                                    "VS Code: Error automatically installing extension: {}",
                                    e
                                ));
                                results.push(InstallResult {
                                    changed: false,
                                    diff: None,
                                    message: "VS Code: Unable to automatically install extension. Please cmd+click on the following link to install: vscode:extension/git-ai.git-ai-vscode (or navigate to https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode in your browser)".to_string(),
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    results.push(InstallResult {
                        changed: false,
                        diff: None,
                        message: format!("VS Code: Failed to check extension: {}", e),
                    });
                }
            }
        } else {
            results.push(InstallResult {
                changed: false,
                diff: None,
                message: "VS Code: Unable to automatically install extension. Please cmd+click on the following link to install: vscode:extension/git-ai.git-ai-vscode (or navigate to https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode in your browser)".to_string(),
            });
        }

        // Configure git.path
        {
            use crate::mdm::utils::{
                git_shim_path_string, update_git_path_setting, update_vscode_chat_hook_settings,
            };

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
                                "VS Code: git.path updated in {}",
                                settings_path.display()
                            ),
                        });
                    }
                    Ok(None) => {
                        results.push(InstallResult {
                            changed: false,
                            diff: None,
                            message: format!(
                                "VS Code: git.path already configured in {}",
                                settings_path.display()
                            ),
                        });
                    }
                    Err(e) => {
                        results.push(InstallResult {
                            changed: false,
                            diff: None,
                            message: format!("VS Code: Failed to configure git.path: {}", e),
                        });
                    }
                }

                match update_vscode_chat_hook_settings(&settings_path, dry_run) {
                    Ok(Some(diff)) => {
                        results.push(InstallResult {
                            changed: true,
                            diff: Some(diff),
                            message: format!(
                                "VS Code: chat hook settings updated in {}",
                                settings_path.display()
                            ),
                        });
                    }
                    Ok(None) => {
                        results.push(InstallResult {
                            changed: false,
                            diff: None,
                            message: format!(
                                "VS Code: chat hook settings already configured in {}",
                                settings_path.display()
                            ),
                        });
                    }
                    Err(e) => {
                        results.push(InstallResult {
                            changed: false,
                            diff: None,
                            message: format!(
                                "VS Code: Failed to configure chat hook settings: {}",
                                e
                            ),
                        });
                    }
                }
            }
        }

        Ok(results)
    }

    fn uninstall_extras(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Vec<UninstallResult>, GitAiError> {
        // Note: Extension must be uninstalled manually
        Ok(vec![UninstallResult {
            changed: false,
            diff: None,
            message: "VS Code: Extension must be uninstalled manually through the editor"
                .to_string(),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vscode_installer_name() {
        let installer = VSCodeInstaller;
        assert_eq!(installer.name(), "VS Code");
    }

    #[test]
    fn test_vscode_installer_id() {
        let installer = VSCodeInstaller;
        assert_eq!(installer.id(), "vscode");
    }

    #[test]
    fn test_vscode_settings_targets() {
        let targets = VSCodeInstaller::settings_targets();
        // Should return paths for Code and Code - Insiders
        assert!(!targets.is_empty());
        // Targets should contain some known VSCode paths
        let targets_str: Vec<String> = targets.iter().map(|p| p.display().to_string()).collect();
        let has_code_path = targets_str
            .iter()
            .any(|s| s.contains("Code") || s.contains("code"));
        assert!(has_code_path, "Should include VSCode-related paths");
    }

    #[test]
    fn test_vscode_uninstall_extras_returns_manual_message() {
        let installer = VSCodeInstaller;
        let params = HookInstallerParams {
            binary_path: std::path::PathBuf::from("/usr/local/bin/git-ai"),
        };

        let results = installer.uninstall_extras(&params, false).unwrap();
        assert_eq!(results.len(), 1);
        assert!(!results[0].changed);
        assert!(results[0].message.contains("manually"));
    }

    #[test]
    fn test_vscode_install_hooks_returns_none() {
        let installer = VSCodeInstaller;
        let params = HookInstallerParams {
            binary_path: std::path::PathBuf::from("/usr/local/bin/git-ai"),
        };

        // install_hooks should return None because VSCode uses extension, not config hooks
        let result = installer.install_hooks(&params, false).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_vscode_uninstall_hooks_returns_none() {
        let installer = VSCodeInstaller;
        let params = HookInstallerParams {
            binary_path: std::path::PathBuf::from("/usr/local/bin/git-ai"),
        };

        let result = installer.uninstall_hooks(&params, false).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_vscode_settings_targets_returns_candidates() {
        let targets = VSCodeInstaller::settings_targets();
        assert!(!targets.is_empty());
    }
}
