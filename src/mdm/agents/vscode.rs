use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams, InstallResult, UninstallResult};
use crate::mdm::utils::{
    binary_exists, get_binary_version, home_dir, install_vsc_editor_extension,
    is_github_codespaces, is_vsc_editor_extension_installed, parse_version,
    settings_paths_for_products, should_process_settings_target, version_meets_requirement,
    MIN_CODE_VERSION,
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
        let has_binary = binary_exists("code");
        let has_dotfiles = home_dir().join(".vscode").exists();
        let has_settings_targets = Self::settings_targets()
            .iter()
            .any(|path| should_process_settings_target(path));

        if !has_binary && !has_dotfiles && !has_settings_targets {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        // If we have the binary, check version
        if has_binary
            && let Ok(version_str) = get_binary_version("code")
                && let Some(version) = parse_version(&version_str)
                    && !version_meets_requirement(version, MIN_CODE_VERSION) {
                        return Err(GitAiError::Generic(format!(
                            "VS Code version {}.{} detected, but minimum version {}.{} is required",
                            version.0, version.1, MIN_CODE_VERSION.0, MIN_CODE_VERSION.1
                        )));
                    }

        // VS Code hooks are installed via extension, not config files
        // Check if extension is installed
        if binary_exists("code") {
            match is_vsc_editor_extension_installed("code", "git-ai.git-ai-vscode") {
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
        if binary_exists("code") {
            match is_vsc_editor_extension_installed("code", "git-ai.git-ai-vscode") {
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
                        match install_vsc_editor_extension("code", "git-ai.git-ai-vscode") {
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

        // Configure git.path on Windows
        #[cfg(windows)]
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
                            message: format!("VS Code: git.path updated in {}", settings_path.display()),
                        });
                    }
                    Ok(None) => {
                        results.push(InstallResult {
                            changed: false,
                            diff: None,
                            message: format!("VS Code: git.path already configured in {}", settings_path.display()),
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
            message: "VS Code: Extension must be uninstalled manually through the editor".to_string(),
        }])
    }
}
